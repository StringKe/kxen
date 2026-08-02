// ---------------- inbox ----------------

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::core::session::now_ms;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct InboxEntry {
    pub(super) from: String,
    pub(super) text: String,
    /// lead transcript 使用稳定 ID。JSONL 已提交但 meta 更新失败时，同一 mailbox 记录重放不会重复追加。
    #[serde(default)]
    pub(super) transcript_id: String,
    #[serde(default)]
    pub(super) at: u64,
}

/// 按 inbox 文件路径分桶的写锁：append 与 drain 必须互斥，
/// 否则 drain 的「读 -> 清空」窗口会吞掉并发 append（读旧文 -> append 写入 -> 清空覆盖）。
static INBOX_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    // shared::lock 容错取锁（P2-7）：持锁线程 panic 毒化不代表数据损坏，expect 会把整个 team 收件通道打死
    crate::core::shared::lock(INBOX_LOCKS.get_or_init(|| Mutex::new(HashMap::new())))
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Team Session 生命周期终点：该目录不会再接收 inbox 写入后回收路径锁。
pub(super) fn drop_session_locks(session_dir: &Path) {
    if let Some(locks) = INBOX_LOCKS.get() {
        crate::core::shared::lock(&locks).retain(|path, _| !path.starts_with(session_dir));
    }
}

/// 单条文本上限：inbox 是落盘 mailbox，无 cap 时失控/恶意写入可让单条无限膨胀
///（drain 后整条进 LLM 历史，超限文本还会爆上下文）。截断保留前缀并标注原始长度。
/// append 侧不做文件总量 cap：inbox 读后即焚（drain 即清空），总量已被消费节奏自然限制。
pub(super) const INBOX_TEXT_CAP: usize = 4000;

pub(super) fn append_inbox(dir: &Path, to: &str, from: &str, text: &str) -> Result<(), String> {
    append_entry(
        dir,
        to,
        &InboxEntry { from: from.to_string(), text: cap_text(text), transcript_id: crate::core::ids::new_id("msg"), at: now_ms() },
    )
}

pub(super) fn restore_inbox(dir: &Path, to: &str, entry: &InboxEntry) -> Result<(), String> {
    append_entry(dir, to, entry)
}

fn append_entry(dir: &Path, to: &str, entry: &InboxEntry) -> Result<(), String> {
    use std::io::Write;
    crate::core::ids::validate_id(to)?;
    crate::core::ids::validate_id(&entry.from)?;
    let path = dir.join("inboxes").join(format!("{to}.json"));
    let lock = lock_for(&path);
    let _guard = lock.lock().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().expect("inbox path has a parent")).map_err(|error| error.to_string())?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    writeln!(file, "{}", serde_json::to_string(entry).map_err(|error| error.to_string())?).map_err(|e| e.to_string())?;
    file.sync_data().map_err(|error| error.to_string())
}

/// 按 char 计数截断（不劈 UTF-8 边界），超限标注原始长度让收信方知道看的是残篇
fn cap_text(text: &str) -> String {
    let total = text.chars().count();
    if total <= INBOX_TEXT_CAP {
        return text.to_string();
    }
    let kept: String = text.chars().take(INBOX_TEXT_CAP).collect();
    format!("{kept}...[truncated, original {total} chars]")
}

/// 读 + 校验 + 清空。任一坏行使整批 fail closed，原文件保持不变，避免清空时永久丢失损坏行。
/// 临界区覆盖完整「读-校验-清空」：append 不会落在读取与清空的间隙里。
pub(super) fn drain_inbox(dir: &Path, name: &str) -> Result<Vec<(String, String)>, String> {
    drain_inbox_entries(dir, name).map(|entries| entries.into_iter().map(|entry| (entry.from, entry.text)).collect())
}

pub(super) fn drain_inbox_entries(dir: &Path, name: &str) -> Result<Vec<InboxEntry>, String> {
    crate::core::ids::validate_id(name)?;
    let path = dir.join("inboxes").join(format!("{name}.json"));
    let lock = lock_for(&path);
    let _guard = lock.lock().map_err(|error| format!("lock inbox {name}: {error}"))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read inbox {}: {error}", path.display())),
    };
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut entry = serde_json::from_str::<InboxEntry>(line)
            .map_err(|error| format!("parse inbox {} line {}: {error}", path.display(), index + 1))?;
        if entry.transcript_id.is_empty() {
            entry.transcript_id = crate::core::ids::new_id("msg");
        }
        out.push(entry);
    }
    // 未提交清空时不得交付，否则下一次 drain 会重复注入同一批消息。
    clear_atomic(&path)?;
    Ok(out)
}

fn clear_atomic(path: &Path) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    file.write_all(b"").map_err(|error| format!("write {}: {error}", tmp.display()))?;
    file.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单条 cap：超限截断并标注原始长度，未超限原样通过
    #[test]
    fn append_caps_oversized_text() {
        let dir = std::env::temp_dir().join(format!("kxen-inbox-cap-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inboxes")).unwrap();
        let big = "x".repeat(9000);
        append_inbox(&dir, "a", "w", &big).unwrap();
        append_inbox(&dir, "a", "w", "short").unwrap();
        let got = drain_inbox(&dir, "a").unwrap();
        assert_eq!(got.len(), 2);
        assert!(got[0].1.len() < INBOX_TEXT_CAP + 64, "截断后必须贴近 cap: {}", got[0].1.len());
        assert!(got[0].1.ends_with("original 9000 chars]"), "截断必须标注原始长度");
        assert!(got[0].1.starts_with(&"x".repeat(100)), "前缀内容必须保留");
        assert_eq!(got[1].1, "short", "未超限文本原样通过");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 并发 append/drain 零丢失零重复：每条消息恰好被 drain 到一次。
    #[test]
    fn concurrent_append_and_drain_lose_nothing() {
        let dir = std::env::temp_dir().join(format!("kxen-inbox-race-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inboxes")).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drained = Arc::new(Mutex::new(Vec::<String>::new()));

        let mut writers = Vec::new();
        for t in 0..4 {
            let dir = dir.clone();
            writers.push(std::thread::spawn(move || {
                for i in 0..25 {
                    append_inbox(&dir, "a", "w", &format!("t{t}-m{i}")).unwrap();
                }
            }));
        }
        let drainer = {
            let dir = dir.clone();
            let stop = stop.clone();
            let drained = drained.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    for (_, text) in drain_inbox(&dir, "a").unwrap() {
                        drained.lock().unwrap().push(text);
                    }
                    std::thread::yield_now();
                }
            })
        };
        for w in writers {
            w.join().unwrap();
        }
        // 写入全部完成后收尾排空，再停 drainer
        for (_, text) in drain_inbox(&dir, "a").unwrap() {
            drained.lock().unwrap().push(text);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        drainer.join().unwrap();
        // join 后可能还有最后一轮 drain 遗漏：再收一次尾
        for (_, text) in drain_inbox(&dir, "a").unwrap() {
            drained.lock().unwrap().push(text);
        }

        let got = drained.lock().unwrap();
        assert_eq!(got.len(), 100, "零丢失零重复：4 x 25 条必须恰好各到一次");
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 100, "重复投递检测");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_lock_entries_are_reclaimable() {
        let base = std::env::temp_dir().join(format!("kxen-inbox-lifecycle-{}", std::process::id()));
        let first = base.join("first");
        let second = base.join("second");
        std::fs::create_dir_all(first.join("inboxes")).unwrap();
        std::fs::create_dir_all(second.join("inboxes")).unwrap();
        append_inbox(&first, "lead", "worker", "one").unwrap();
        append_inbox(&second, "lead", "worker", "two").unwrap();

        drop_session_locks(&first);
        let locks = crate::core::shared::lock(INBOX_LOCKS.get().unwrap());
        assert!(!locks.keys().any(|path| path.starts_with(&first)));
        assert!(locks.keys().any(|path| path.starts_with(&second)));
        drop(locks);

        drop_session_locks(&second);
        std::fs::remove_dir_all(base).ok();
    }

    /// P2-7 poison 容错回归：锁表被持锁 panic 毒化后，lock_for 不得 panic（expect 版会把
    /// team 收件通道永久打死）。本测试把全局锁表毒化留在进程内：其余触及该表的路径必须全部
    /// 走 shared::lock（drop_session_locks 与上方回收测试同口径），否则并发下会被本测试拖挂。
    #[test]
    fn poisoned_locks_map_still_usable() {
        let locks = INBOX_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = crate::core::shared::lock(locks);
            panic!("poison inbox locks map");
        }));
        assert!(locks.is_poisoned(), "前置：锁表必须已毒化");
        let lock = lock_for(Path::new("/tmp/kxen-inbox-poison-test.json"));
        let _guard = crate::core::shared::lock(&lock);
    }
}

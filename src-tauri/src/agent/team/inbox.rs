// ---------------- inbox ----------------

use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::member_loop::now_ms;

#[derive(Debug, Deserialize)]
struct InboxEntry {
    from: String,
    text: String,
    #[serde(default)]
    #[allow(dead_code)]
    at: u64,
}

/// 按 inbox 文件路径分桶的写锁：append 与 drain 必须互斥，
/// 否则 drain 的「读 -> 清空」窗口会吞掉并发 append（读旧文 -> append 写入 -> 清空覆盖）。
static INBOX_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    INBOX_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("inbox locks")
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Team Session 生命周期终点：该目录不会再接收 inbox 写入后回收路径锁。
pub(super) fn drop_session_locks(session_dir: &Path) {
    if let Some(locks) = INBOX_LOCKS.get() {
        locks.lock().expect("inbox locks").retain(|path, _| !path.starts_with(session_dir));
    }
}

/// 单条文本上限：inbox 是落盘 mailbox，无 cap 时失控/恶意写入可让单条无限膨胀
///（drain 后整条进 LLM 历史，超限文本还会爆上下文）。截断保留前缀并标注原始长度。
/// append 侧不做文件总量 cap：inbox 读后即焚（drain 即清空），总量已被消费节奏自然限制。
pub(super) const INBOX_TEXT_CAP: usize = 4000;

pub(super) fn append_inbox(dir: &Path, to: &str, from: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    let path = dir.join("inboxes").join(format!("{to}.json"));
    let entry = json!({ "from": from, "text": cap_text(text), "at": now_ms() });
    let lock = lock_for(&path);
    let _guard = lock.lock().map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| e.to_string())?;
    writeln!(file, "{}", entry).map_err(|e| e.to_string())
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

/// 读 + 校验 + 清空（坏行报错剔除，valid 照常送达——对齐 Claude Code v2.1.207+ 行为）。
/// 临界区覆盖完整「读-校验-清空」：append 不会落在读取与清空的间隙里。
pub(super) fn drain_inbox(dir: &Path, name: &str) -> Vec<(String, String)> {
    let path = dir.join("inboxes").join(format!("{name}.json"));
    let lock = lock_for(&path);
    let _guard = match lock.lock() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(inbox = name, error = %e, "inbox lock poisoned");
            return Vec::new();
        }
    };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        match serde_json::from_str::<InboxEntry>(line) {
            Ok(entry) => out.push((entry.from, entry.text)),
            Err(e) => tracing::warn!(inbox = name, error = %e, "dropping malformed inbox entry"),
        }
    }
    let _ = std::fs::write(&path, "");
    out
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
        let got = drain_inbox(&dir, "a");
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
                    for (_, text) in drain_inbox(&dir, "a") {
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
        for (_, text) in drain_inbox(&dir, "a") {
            drained.lock().unwrap().push(text);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        drainer.join().unwrap();
        // join 后可能还有最后一轮 drain 遗漏：再收一次尾
        for (_, text) in drain_inbox(&dir, "a") {
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
        let locks = INBOX_LOCKS.get().unwrap().lock().unwrap();
        assert!(!locks.keys().any(|path| path.starts_with(&first)));
        assert!(locks.keys().any(|path| path.starts_with(&second)));
        drop(locks);

        drop_session_locks(&second);
        std::fs::remove_dir_all(base).ok();
    }
}

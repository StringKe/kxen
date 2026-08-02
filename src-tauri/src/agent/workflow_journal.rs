//! workflow journal：agent 派发结果按 run_id 落盘，同 run_id 重跑自动跳过已完成项（resume）。
//! 文件：data_dir/workflow-journals/<run_id>.jsonl（每行 {key, result, ts}）。
//! 命名空间隔离：ns = sha256(run_id, sha256(script))，key = sha256(ns, role, prompt)——
//! 同 run_id 换了脚本语义全变，旧条目必须 miss；脚本哈希进 ns 让缓存自动失效。

use sha2::Digest;
use std::collections::HashMap;
use std::path::PathBuf;

/// 条目 TTL 7 天：resume 场景是「崩溃/取消后接着跑」，跨度按天计；
/// 超期条目命中率趋零却无限涨盘，且脚本产物时效性丧失，留之无益。
const ENTRY_TTL_SECS: u64 = 7 * 24 * 3600;

fn journal_file(run_id: &str) -> PathBuf {
    crate::core::paths::data_dir().join("workflow-journals").join(format!("{run_id}.jsonl"))
}

/// 多段稳定哈希：段间写 0 分隔符（hex 输出无 0 字节，拼接防 ("ab","c") 与 ("a","bc") 撞车）。
fn stable_hash(segments: &[&str]) -> String {
    let mut h = sha2::Sha256::new();
    for seg in segments {
        h.update(seg.as_bytes());
        h.update([0u8]);
    }
    format!("{:x}", h.finalize())
}

pub struct Journal {
    ns: String,
    done: HashMap<String, String>,
    file: PathBuf,
}

impl Journal {
    /// run_id 会拼进 journal 文件路径：非法 id（路径穿越）直接返回 None（放弃 resume）。
    /// 打开即清理：超 TTL 条目 + 坏行（含无 ts 旧格式）剔除；有丢弃才重写文件。
    pub fn open(run_id: &str, script: &str) -> Option<Self> {
        crate::core::ids::validate_id(run_id).ok()?;
        let ns = stable_hash(&[run_id, &stable_hash(&[script])]);
        let file = journal_file(run_id);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut done = HashMap::new();
        let mut kept_lines: Vec<String> = Vec::new();
        let mut dropped = false;
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                let entry = serde_json::from_str::<serde_json::Value>(line).ok();
                let alive = entry
                    .as_ref()
                    .and_then(|v| {
                        Some((v.get("key")?.as_str()?.to_string(), v.get("result")?.as_str()?.to_string(), v.get("ts")?.as_u64()?))
                    })
                    .filter(|(_, _, ts)| now.saturating_sub(*ts) <= ENTRY_TTL_SECS);
                match alive {
                    Some((key, result, _)) => {
                        done.insert(key, result);
                        kept_lines.push(line.to_string());
                    }
                    None => dropped = true,
                }
            }
        }
        if dropped {
            // tmp+rename 原子写：清理重写若留半截文件，下轮 open 会把存活条目一并剔除（缓存全 miss）
            let tmp = file.with_extension("jsonl.tmp");
            let text = kept_lines.join("\n") + if kept_lines.is_empty() { "" } else { "\n" };
            if let Err(e) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &file)) {
                tracing::warn!(error = %e, "workflow journal purge rewrite failed");
            }
        }
        Some(Self { ns, done, file })
    }

    /// 宿主命名空间版 open（P2：workflow run_id 完全由模型参数决定的修复）：模型传入的
    /// run_id 只作当前会话内的 resume 令牌，真实 journal id = sha256(session, run_id)——
    /// 跨会话/历史同 run_id 不再命中旧 journal 跳过真实派发；同会话同脚本重跑仍可断点续跑。
    pub fn open_scoped(session_id: Option<&str>, run_id: &str, script: &str) -> Option<Self> {
        let scoped = stable_hash(&[session_id.unwrap_or("no-session"), run_id]);
        Self::open(&scoped, script)
    }

    /// 已完成的派发结果（resume 命中则免重跑）。
    pub fn cached(&self, role: &str, prompt: &str) -> Option<&String> {
        self.done.get(&stable_hash(&[&self.ns, role, prompt]))
    }

    /// 追加一条完成记录（立即落盘，崩溃可续）。
    pub fn record(&mut self, role: &str, prompt: &str, result: &str) {
        use std::io::Write;
        let key = stable_hash(&[&self.ns, role, prompt]);
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if let Some(parent) = self.file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.file) {
            let line = serde_json::json!({ "key": key, "result": result, "ts": ts });
            let _ = writeln!(f, "{line}");
        }
        self.done.insert(key, result.to_string());
    }

    pub fn completed(&self) -> usize {
        self.done.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup(run_id: &str) {
        let _ = std::fs::remove_file(journal_file(run_id));
    }

    #[test]
    fn record_and_resume_hit() {
        let run_id = format!("test-hit-{}", std::process::id());
        cleanup(&run_id);
        {
            let mut j = Journal::open(&run_id, "script-v1").unwrap();
            assert_eq!(j.completed(), 0);
            j.record("execution", "do A", "result A");
        }
        // 重新打开（模拟崩溃后 resume）：同脚本命中缓存
        let j2 = Journal::open(&run_id, "script-v1").unwrap();
        assert_eq!(j2.completed(), 1);
        assert_eq!(j2.cached("execution", "do A").map(String::as_str), Some("result A"));
        cleanup(&run_id);
    }

    #[test]
    fn script_change_invalidates_cache() {
        let run_id = format!("test-script-{}", std::process::id());
        cleanup(&run_id);
        {
            let mut j = Journal::open(&run_id, "script-v1").unwrap();
            j.record("execution", "do A", "result A");
        }
        // 同 run_id 不同脚本：ns 变了，旧条目必须 miss（脚本语义全变，回缓存=跑错任务）
        let j2 = Journal::open(&run_id, "script-v2").unwrap();
        assert_eq!(j2.cached("execution", "do A"), None);
        cleanup(&run_id);
    }

    #[test]
    fn input_change_is_miss() {
        let run_id = format!("test-input-{}", std::process::id());
        cleanup(&run_id);
        let mut j = Journal::open(&run_id, "script-v1").unwrap();
        j.record("execution", "do A", "result A");
        assert_eq!(j.cached("execution", "do B"), None, "prompt 变了必须 miss");
        assert_eq!(j.cached("review", "do A"), None, "role 变了必须 miss");
        cleanup(&run_id);
    }

    #[test]
    fn expired_and_malformed_entries_are_purged_on_open() {
        let run_id = format!("test-ttl-{}", std::process::id());
        let file = journal_file(&run_id);
        cleanup(&run_id);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let stale = now - ENTRY_TTL_SECS - 1;
        let mut j = Journal::open(&run_id, "script-v1").unwrap();
        j.record("execution", "fresh", "ok");
        // 手工塞入：过期条目 + 无 ts 旧格式 + 坏行
        let text = std::fs::read_to_string(&file).unwrap();
        let polluted = format!(
            "{text}{{\"key\":\"stale\",\"result\":\"old\",\"ts\":{stale}}}\n{{\"key\":\"legacy\",\"result\":\"no-ts\"}}\nnot json\n"
        );
        std::fs::write(&file, polluted).unwrap();

        let j2 = Journal::open(&run_id, "script-v1").unwrap();
        assert_eq!(j2.completed(), 1, "过期/旧格式/坏行必须剔除");
        assert_eq!(j2.cached("execution", "fresh").map(String::as_str), Some("ok"));
        // 有丢弃才重写：文件里只剩存活行
        let rewritten = std::fs::read_to_string(&file).unwrap();
        assert!(!rewritten.contains("stale"));
        assert!(!rewritten.contains("legacy"));
        assert!(!rewritten.contains("not json"));
        assert_eq!(rewritten.lines().count(), 1);
        assert!(!file.with_extension("jsonl.tmp").exists(), "清理重写必须走 tmp+rename，不留残骸");
        cleanup(&run_id);
    }

    #[test]
    fn invalid_run_id_yields_none() {
        // 路径穿越式 run_id 不得拼进文件路径
        assert!(Journal::open("../escape", "s").is_none());
        assert!(Journal::open("a/b", "s").is_none());
        assert!(Journal::open("", "s").is_none());
    }

    #[test]
    fn scoped_run_id_isolates_sessions_but_resumes_within_session() {
        // 模型给出的同一 run_id：会话 B 不得命中会话 A 的旧 journal（跳过真实派发=跑错任务）；
        // 同会话重开（崩溃/取消后 resume）必须照常命中。
        let run_id = format!("test-scoped-{}", std::process::id());
        let file_a = journal_file(&stable_hash(&["sess-a", &run_id]));
        let file_b = journal_file(&stable_hash(&["sess-b", &run_id]));
        let _ = std::fs::remove_file(&file_a);
        let _ = std::fs::remove_file(&file_b);

        {
            let mut ja = Journal::open_scoped(Some("sess-a"), &run_id, "script-v1").unwrap();
            ja.record("execution", "do A", "result A");
        }
        // 同会话 resume：命中
        let ja2 = Journal::open_scoped(Some("sess-a"), &run_id, "script-v1").unwrap();
        assert_eq!(ja2.cached("execution", "do A").map(String::as_str), Some("result A"), "同会话 resume 必须命中");
        // 跨会话同 run_id 同脚本：miss（模型参数不能直接命中旧 journal）
        let jb = Journal::open_scoped(Some("sess-b"), &run_id, "script-v1").unwrap();
        assert_eq!(jb.cached("execution", "do A"), None, "跨会话同 run_id 不得命中");
        // 无会话上下文与有会话上下文也互不相同
        let jn = Journal::open_scoped(None, &run_id, "script-v1").unwrap();
        assert_eq!(jn.cached("execution", "do A"), None, "无会话命名空间不得命中");

        let _ = std::fs::remove_file(&file_a);
        let _ = std::fs::remove_file(&file_b);
        let _ = std::fs::remove_file(journal_file(&stable_hash(&["no-session", &run_id])));
    }
}

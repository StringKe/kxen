//! file-backed id 的统一生成与校验。
//! 这些 id 会拼进文件路径（session meta/JSONL、team 目录、inbox、workflow journal），
//! 生成端必须抗碰撞，校验端必须杜绝路径穿越。

/// id 长度上限：新格式 `<prefix>_<uuid32>` 约 40 字符，上限放宽以兼容可能的存量数据。
pub const MAX_ID_LEN: usize = 128;

/// 生成 opaque id：`<prefix>_<uuid v4 无连字符>`。
/// 毫秒+进程号方案在同毫秒高频创建（message/approval/schedule）下会碰撞，故用 uuid v4。
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// 白名单校验 [A-Za-z0-9_-]：点和斜杠都不在白名单内，`..` 与路径分隔符被天然拒绝。
/// 旧格式 id（前缀_毫秒_进程号）只含白名单字符，磁盘存量数据保持可读。
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 外部输入（RPC 参数 / 工具参数）拼进文件路径前必须先过这层。
pub fn validate_id(id: &str) -> Result<(), String> {
    if is_valid_id(id) { Ok(()) } else { Err(format!("invalid id: {id:?}")) }
}

/// validate_id 的 io 变体：落盘前校验失败直接以 InvalidInput 传播，调用点免手写错误映射。
pub fn validate_id_io(id: &str) -> std::io::Result<()> {
    validate_id(id).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_id_is_prefixed_unique_and_valid() {
        let a = new_id("ses");
        let b = new_id("ses");
        assert!(a.starts_with("ses_"));
        assert_ne!(a, b, "同毫秒连续生成不得碰撞");
        assert!(is_valid_id(&a));
        assert_eq!(a.len(), "ses_".len() + 32);
    }

    #[test]
    fn legacy_ms_pid_ids_stay_valid() {
        // 磁盘上已存在的旧格式 id（毫秒+进程号）必须仍可读
        for legacy in [
            "ses_1735689600000_1a2b",
            "msg_1735689600000_00ff",
            "cron-1735689600000-00ff",
            "appr_1735689600000_00ff",
            "goal_1735689600000_00ff1e",
            "task_1735689600000_00ff1e",
        ] {
            assert!(is_valid_id(legacy), "旧格式 id 应保持可读: {legacy}");
        }
    }

    #[test]
    fn rejects_traversal_and_malformed() {
        for bad in ["", "..", "../escape", "a/b", "a\\b", "a b", "a.b", "a:b", "中文字符"] {
            assert!(!is_valid_id(bad), "应拒绝: {bad:?}");
            assert!(validate_id(bad).is_err());
        }
        let overlong = "x".repeat(MAX_ID_LEN + 1);
        assert!(!is_valid_id(&overlong), "超长 id 应拒绝");
    }
}

//! 规则模式与常量（F1-F5 规则族的匹配表）。

use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny {
        rule_id: &'static str,
        reason: Cow<'static, str>,
        suggestion: Option<&'static str>,
    },
    /// 需用户审批后放行（push --force / reset --hard / sudo 等高危但合法操作）
    Ask {
        reason: Cow<'static, str>,
    },
    /// trash 的可恢复删除（approval 档，safety 不硬拦但记录）
    Recoverable,
}

// 高危但合法：走 ask-user 审批档
pub(super) static ASK_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (Regex::new(r"\bgit\s+push\b[^|;]*\s(-f\b|--force\b)").unwrap(), "git push --force 覆盖远端历史"),
        // 只拦裸 reset --hard（丢弃全部未提交改动）；带 ref 的是常用安全操作（既有测试约定）
        (Regex::new(r"^\s*git\s+reset\s+--hard\s*$").unwrap(), "git reset --hard 丢弃未提交改动"),
        (Regex::new(r"^\s*sudo\b").unwrap(), "sudo 提权执行"),
        (Regex::new(r"\bgit\s+clean\s+-[a-z]*f").unwrap(), "git clean 删除未跟踪文件（不可恢复）"),
        (Regex::new(r"\bkill\s+-9\b|\bkill\s+-KILL\b").unwrap(), "kill -9 强制终止进程（不可捕获）"),
        (Regex::new(r"\bbrew\s+uninstall\b").unwrap(), "brew uninstall 卸载软件包"),
        (Regex::new(r"\bnpm\s+(uninstall|publish)\b").unwrap(), "npm uninstall/publish 变更包状态"),
        (Regex::new(r"\bchmod\s+-R\b").unwrap(), "chmod -R 递归改权限"),
    ]
});

// F1 系统路径（macOS 细化：/private/var/folders 与 /private/tmp 是临时区，豁免）
pub(super) const SYSTEM_PATHS: &[&str] = &[
    "/",
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/var",
    "/Library",
    "/private/etc",
    "/private/var/db",
    "/private/var/root",
    "/private/bin",
    "/private/sbin",
    "/private/System",
    "/boot",
    "/proc",
    "/sys",
    "/dev",
];

pub(super) const EXEMPT_PREFIXES: &[&str] =
    &["/private/var/folders", "/private/tmp", "/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

pub(super) fn home_top() -> &'static [&'static str] {
    &["Documents", "Desktop", "Downloads", "Library", "Pictures", "Movies"]
}

pub(super) fn home_credential_dot() -> &'static [&'static str] {
    // 与 path_policy::sensitive_reason 的凭证目录清单保持一致（两处任一漏项都是绕过通道）
    &[".ssh", ".gnupg", ".aws", ".kube", ".docker", ".codex", ".claude", ".grok", ".kimi-code"]
}

pub(super) static DISK_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"^\s*/?dd\b.*\bof=/dev/", r"\bmkfs(\.|\b)", r"\bdiskutil\s+erase", r"\bhdiutil\s+erase", r"\bfdisk\b", r"\bparted\b"]
        .iter()
        .map(|p| Regex::new(p).expect("static pattern"))
        .collect()
});

pub(super) static SYSTEM_CMDS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"\b(shutdown|reboot|halt)\b", r"\b(nvram|csrutil)\b"].iter().map(|p| Regex::new(p).expect("static pattern")).collect()
});

pub(super) static CRED_CMDS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [r"\bsecurity\s+delete-", r"\bgpg\s+--delete-secret-key"].iter().map(|p| Regex::new(p).expect("static pattern")).collect()
});

pub(super) static DESTROY_CMDS: LazyLock<Vec<(Regex, &'static str, &'static str)>> = LazyLock::new(|| {
    [
        (r"\bterraform\s+destroy\b", "F4", "terraform destroy 销毁基础设施"),
        (r"\bdropdb\b", "F4", "dropdb 删除整个数据库"),
        (r"\b(psql|mysql|mongosh?|mongo|redis-cli)\b.*\b(drop\s+database|dropDatabase|flushall)", "F4", "数据库毁灭操作"),
        (r"\bkubectl\s+delete\s+(ns|namespace|--all)\b", "F4", "kubectl 命名空间/全量删除"),
        (r"\baws\s+s3\s+rb\s+.*--force\b", "F4", "aws s3 rb --force 删除整个 bucket"),
        (r"\bgcloud\s+projects\s+delete\b", "F4", "gcloud 项目删除"),
        (r"\bdocker\s+system\s+prune\b.*(--volumes|-a\b)", "F4", "docker system prune 卷/全量清理"),
    ]
    .iter()
    .map(|(p, id, why)| (Regex::new(p).expect("static pattern"), *id, *why))
    .collect()
});

pub(super) static GIT_DESTROY: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [(r"\bgit\s+update-ref\s+-d\b", "git update-ref -d 删除 refs"), (r"\bgit\s+branch\s+-D\s+\*", "git branch -D 批量删除分支")]
        .iter()
        .map(|(p, why)| (Regex::new(p).expect("static pattern"), *why))
        .collect()
});

pub(super) static VAR_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$\{?[A-Za-z_][A-Za-z0-9_]*\}?").unwrap());

pub(super) static GIT_SEGMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|/)\.git(/|$)").unwrap());

pub(super) const DELETE_CMDS: &[&str] = &["rm", "rmdir", "trash", "unlink", "shred"];
pub(super) const MOVE_CMDS: &[&str] = &["mv", "move"];

pub(super) fn deny(rule_id: &'static str, reason: impl Into<Cow<'static, str>>, suggestion: Option<&'static str>) -> Verdict {
    Verdict::Deny { rule_id, reason: reason.into(), suggestion }
}

//! 隔离的 fresh shell：不加载 login/rc 文件，也不回放用户 alias/function。
//! 包装层重复执行 safety 判定，避免无意绕过上层 gate 的直调路径。

use crate::tools::safety::{Verdict, evaluate_shell_command};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

impl ShellKind {
    pub fn binary(&self) -> &'static str {
        match self {
            ShellKind::Zsh => "/bin/zsh",
            ShellKind::Bash => "/bin/bash",
            // Apple Silicon Homebrew 路径（/usr/local 是 Intel 残留）
            ShellKind::Fish => "/opt/homebrew/bin/fish",
        }
    }
}

/// 把用户命令包装为「稳定 PATH + cd + 命令遮蔽 + 命令」的 fresh shell 调用。
pub fn wrap_command(kind: ShellKind, workdir: &str, command: &str) -> Vec<String> {
    if let Verdict::Deny { rule_id, reason, suggestion } = evaluate_shell_command(command, workdir) {
        let hint = suggestion.map(|value| format!(" Suggestion: {value}")).unwrap_or_default();
        let message = format!("blocked by safety rule {rule_id}: {reason}.{hint}");
        let script = format!("printf '%s\\n' {} >&2\nexit 126", shell_escape(&message));
        return vec![kind.binary().to_string(), "-c".to_string(), script];
    }
    let script = format!(
        "{path}\n{speed}\ncd -- {workdir}\n{command}",
        path = path_setup(kind),
        speed = speed_shadow(kind),
        workdir = shell_escape(workdir),
        command = command,
    );
    vec![kind.binary().to_string(), "-c".to_string(), script]
}

fn path_setup(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Fish => "set -gx PATH /opt/homebrew/bin /usr/local/bin /usr/bin /bin /usr/sbin /sbin $PATH",
        _ => "export PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}",
    }
}

/// 提速遮蔽（grok-build 实证）：grep -> ugrep、find -> bfs，两者都是 CLI 兼容替换。
/// 未安装不遮蔽（按 PATH 探测逐命令门控）。
fn speed_shadow(kind: ShellKind) -> String {
    match kind {
        ShellKind::Fish => "if command -vq ugrep; function grep; command ugrep $argv; end; end; if command -vq bfs; function find; command bfs $argv; end; end".into(),
        _ => "command -v ugrep >/dev/null 2>&1 && grep() { command ugrep \"$@\"; }; command -v bfs >/dev/null 2>&1 && find() { command bfs \"$@\"; }; true".into(),
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_contains_shadow_and_cd() {
        let wrapped = wrap_command(ShellKind::Zsh, "/tmp/x", "ls -la");
        assert_eq!(wrapped[0], "/bin/zsh");
        let script = &wrapped[2];
        assert!(script.contains("ugrep"), "should contain grep->ugrep shadow");
        assert!(script.contains("bfs"), "should contain find->bfs shadow");
        assert!(script.contains("cd -- '/tmp/x'"));
        assert!(script.ends_with("ls -la"));
    }

    #[test]
    fn speed_shadow_is_functional() {
        // 真跑一遍：遮蔽脚本不得有语法错误（装了 ugrep/bfs 时函数定义也必须成立）
        let wrapped = wrap_command(ShellKind::Zsh, "/tmp", "type grep >/dev/null; type find >/dev/null");
        let out = std::process::Command::new(&wrapped[0]).args(&wrapped[1..]).output().expect("run zsh");
        assert!(out.status.success(), "shadow script 语法错误: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn permanent_delete_is_blocked_again_at_shell_boundary() {
        let commands = ["rm ./a", "command /bin/rm ./a", "env /usr/bin/unlink ./a", "find . -delete", "sh -c 'rmdir ./a'"];
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            for command in commands {
                let wrapped = wrap_command(kind, "/tmp", command);
                assert!(wrapped[2].contains("exit 126"), "shell boundary must fail closed: {kind:?}: {command}");
                assert!(wrapped[2].contains("delete tool"), "error must guide callers to the recoverable tool");
                assert!(!wrapped[2].ends_with(command), "blocked command must not be appended to the generated script");
            }
        }
    }

    #[test]
    fn blocked_shell_script_does_not_delete_the_target() {
        let dir = std::env::temp_dir().join(format!("kxen-shell-delete-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("keep.txt");
        std::fs::write(&target, "keep").unwrap();
        let command = format!("/bin/rm {}", shell_escape(&target.to_string_lossy()));
        let wrapped = wrap_command(ShellKind::Zsh, dir.to_str().unwrap(), &command);
        let out = std::process::Command::new(&wrapped[0]).args(&wrapped[1..]).output().expect("run zsh");
        assert_eq!(out.status.code(), Some(126));
        assert!(target.exists(), "fail-closed wrapper must preserve the target");
        assert!(String::from_utf8_lossy(&out.stderr).contains("delete tool"));
        trash::delete(&dir).ok();
    }

    #[test]
    fn escape_single_quote() {
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
    }
}

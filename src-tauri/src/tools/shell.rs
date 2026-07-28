//! 隔离的 fresh shell：不加载 login/rc 文件，也不回放用户 alias/function。
//! 每条宿主机命令都必须先经过显式审批；这里仅提供稳定 PATH 与 trash 遮蔽。

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
    let script = format!(
        "{path}\n{shadow}\n{speed}\ncd -- {workdir}\n{command}",
        path = path_setup(kind),
        shadow = trash_shadow(kind),
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

/// rm -> trash 遮蔽（grok-build marker 门控模式）：过滤 rm 的 flags，文件列表进回收站。
fn trash_shadow(kind: ShellKind) -> String {
    match kind {
        ShellKind::Fish => "function rm; for a in $argv; switch $a; case '-*'; ; case '*'; command trash $a; end; end; end".into(),
        _ => "rm() { local args=(); for a in \"$@\"; do case \"$a\" in -*) ;; *) args+=(\"$a\");; esac; done; command trash \"${args[@]}\"; }".into(),
    }
}

/// 提速遮蔽（grok-build 实证）：grep -> ugrep、find -> bfs，两者都是 CLI 兼容替换。
/// 未安装不遮蔽（按 PATH 探测逐命令门控），与 rm->trash 的硬遮蔽不同——trash 是系统自带。
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
        assert!(script.contains("command trash"), "should contain trash shadow");
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
    fn escape_single_quote() {
        assert_eq!(shell_escape("a'b"), "'a'\\''b'");
    }
}

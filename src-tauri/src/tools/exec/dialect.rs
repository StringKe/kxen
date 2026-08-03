use super::{ExecError, ShellKind};
use regex::Regex;
use std::sync::LazyLock;

/// 方言陷阱规则（命中即拒 + 纠正文案）：按 shell 过滤，首个命中返回。
/// 正则预编译；规则只报「该方言下必然/大概率出错」的写法，宁漏勿冤。
static DIALECT_RULES: LazyLock<Vec<(ShellKind, Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (ShellKind::Zsh, Regex::new(r"\[0\]").unwrap(), "zsh arrays are 1-indexed, not 0-indexed."),
        (
            ShellKind::Zsh,
            Regex::new(r"\$\{[A-Za-z_][A-Za-z0-9_]*,+\}").unwrap(),
            "zsh has no ${var,,} case expansion (bash 4+). Use ${(L)var} / ${(U)var}.",
        ),
        (
            ShellKind::Zsh,
            Regex::new(r"(?:^|\s)=[A-Za-z]").unwrap(),
            "zsh expands `=cmd` to the full path of cmd. Quote it or write the command literally.",
        ),
        // $files[@] / ${=files} 是 zsh 合法拆分写法，只拦裸 $var / ${var}（RE2 无 lookahead，用结尾符排除）
        (
            ShellKind::Zsh,
            Regex::new(r"\bfor\s+\w+\s+in\s+(?:\$[A-Za-z_][A-Za-z0-9_]*(?:\s|;|$)|\$\{[A-Za-z_][A-Za-z0-9_]*\})").unwrap(),
            "zsh does not word-split unquoted variables (no sh_word_split): `for x in $var` iterates once. Use ${=var} or an array.",
        ),
        (
            ShellKind::Bash,
            Regex::new(r"\b(?:mapfile|readarray)\b").unwrap(),
            "macOS ships bash 3.2 without mapfile/readarray. Use `while IFS= read -r line; do ...; done`.",
        ),
        (ShellKind::Fish, Regex::new(r"\bexport\s").unwrap(), "fish has no `export`. Use `set -x NAME value`."),
        (ShellKind::Fish, Regex::new(r"\$\(").unwrap(), "fish has no $(...) command substitution. Use (...)."),
        (ShellKind::Fish, Regex::new(r"&&").unwrap(), "fish <3.0 has no &&. Use `cmd1; and cmd2`."),
        (ShellKind::Fish, Regex::new(r"\$\?").unwrap(), "fish has no $?. Use $status."),
    ]
});

/// 方言校验（命中即拒绝 + 纠正文案）。
pub fn validate_dialect(kind: ShellKind, command: &str) -> Result<(), ExecError> {
    for (shell_kind, regex, hint) in DIALECT_RULES.iter() {
        if *shell_kind == kind && regex.is_match(command) {
            return Err(ExecError::Dialect((*hint).to_string()));
        }
    }
    Ok(())
}

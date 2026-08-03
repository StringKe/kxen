//! 命令评估与路径守卫实现。

use regex::Regex;
use std::sync::LazyLock;

mod parse;

use super::rules::{
    ASK_PATTERNS, CRED_CMDS, DELETE_CMDS, DESTROY_CMDS, DISK_PATTERNS, EXEMPT_PREFIXES, GIT_DESTROY, GIT_SEGMENT, MOVE_CMDS, SYSTEM_CMDS,
    SYSTEM_PATHS, VAR_PATTERN, Verdict, deny, home_credential_dot, home_top,
};

/// 主入口：评估一条 shell 命令文本。cwd 用于相对路径解析。
pub fn evaluate_shell_command(command: &str, cwd: &str) -> Verdict {
    let mut recoverable_seen = false;
    let mut ask_seen: Option<Verdict> = None;
    let mut check = |cmd: &str| {
        for tokens in parse::segments(cmd) {
            match eval_segment(&tokens, cwd) {
                v @ Verdict::Deny { .. } => return Some(v),
                v @ Verdict::Ask { .. } => {
                    if ask_seen.is_none() {
                        ask_seen = Some(v);
                    }
                }
                Verdict::Recoverable => recoverable_seen = true,
                Verdict::Allow => {}
            }
        }
        None
    };
    if let Some(v) = check(command) {
        return v;
    }
    // 命令替换（反引号 / $()）内嵌命令同样评估（绕过通道）
    for sub in expand_substitutions(command) {
        if let Some(v) = check(&sub) {
            return v;
        }
    }
    if let Some(v) = ask_seen {
        return v;
    }
    if recoverable_seen { Verdict::Recoverable } else { Verdict::Allow }
}

/// 命令替换展开：反引号与 $() 内嵌的命令同样要进评估（`rm -rf $(cat f)` 类绕过）。
/// $() 用平衡括号扫描：非嵌套正则只捕到第一个 )，嵌套内层命令会整个漏掉；
/// 每个捕获内容再递归展开一次，内层替换里的命令也进评估。
fn expand_substitutions(command: &str) -> Vec<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());
    let mut out: Vec<String> = RE.captures_iter(command).filter_map(|c| c.get(1).map(|m| m.as_str().to_string())).collect();
    let bytes = command.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'(' {
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && depth > 0 {
                match bytes[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if depth == 0 {
                out.push(command[i + 2..j - 1].to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let inner: Vec<String> = out.iter().filter(|s| s.contains("$(") || s.contains('`')).flat_map(|s| expand_substitutions(s)).collect();
    out.extend(inner);
    out
}

fn eval_segment(tokens: &[String], cwd: &str) -> Verdict {
    let seg = tokens.join(" ");
    let cmd_idx = parse::command_index(tokens);
    let cmd_token = tokens.get(cmd_idx).map(String::as_str).unwrap_or("");
    let cmd = parse::command_name(cmd_token);

    if command_is_dynamic(cmd_token) {
        return deny_permanent(format!("命令位置 {cmd_token} 无法静态解析，可能执行不可恢复删除"));
    }
    if cmd == "env" && parse::env_split_requested(tokens, cmd_idx) {
        return deny_permanent("env -S 会重新拆分命令字符串，无法可靠排除不可恢复删除");
    }
    if matches!(cmd, "ash" | "bash" | "dash" | "fish" | "ksh" | "sh" | "zsh") {
        if let Some(script) = parse::nested_script(tokens, cmd_idx) {
            if command_is_dynamic(script) {
                return deny_permanent("嵌套 shell 的脚本来自动态值，无法排除不可恢复删除");
            }
            let verdict = evaluate_shell_command(script, cwd);
            if !matches!(verdict, Verdict::Allow) {
                return verdict;
            }
        } else if !tokens.iter().any(|token| matches!(token.as_str(), "--help" | "--version" | "-n")) {
            return deny_permanent("嵌套 shell 的输入或脚本无法静态检查，无法排除不可恢复删除");
        }
    }
    if cmd == "eval" {
        let script = tokens.get(cmd_idx + 1..).unwrap_or_default().join(" ");
        if script.is_empty() || command_is_dynamic(&script) {
            return deny_permanent("eval 脚本无法静态解析，无法排除不可恢复删除");
        }
        let verdict = evaluate_shell_command(&script, cwd);
        if !matches!(verdict, Verdict::Allow) {
            return verdict;
        }
    }
    if matches!(cmd, "source" | ".") {
        return deny_permanent("source 脚本无法静态检查，无法排除不可恢复删除");
    }
    if DELETE_CMDS.contains(&cmd) && cmd != "trash" {
        return deny_permanent(format!("{cmd} 会执行不可恢复删除"));
    }
    if cmd == "find" && tokens.iter().any(|token| token == "-delete") {
        return deny_permanent("find -delete 会执行不可恢复删除");
    }
    if cmd == "find"
        && let Some(exec_idx) = tokens.iter().position(|token| matches!(token.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"))
    {
        let nested = tokens.get(exec_idx + 1..).unwrap_or_default().join(" ");
        let verdict = evaluate_shell_command(&nested, cwd);
        if matches!(verdict, Verdict::Recoverable) {
            for target in tokens.iter().skip(cmd_idx + 1).take_while(|target| !target.starts_with('-')) {
                if VAR_PATTERN.is_match(target) {
                    return deny("F5", format!("删除目标含未求值变量 {target}，无法静态判定"), Some("使用 delete tool 并明确目标路径"));
                }
                if let Some(blocked) = protected_path_verdict("trash", target, cwd, true) {
                    return blocked;
                }
            }
        }
        if !matches!(verdict, Verdict::Allow) {
            return verdict;
        }
    }
    if cmd == "xargs" {
        let nested_idx = parse::xargs_command_index(tokens, cmd_idx);
        if nested_idx < tokens.len() {
            let verdict = evaluate_shell_command(&tokens[nested_idx..].join(" "), cwd);
            if !matches!(verdict, Verdict::Allow) {
                return verdict;
            }
        }
    }
    if cmd == "rsync" && tokens.iter().any(|token| token == "--delete" || token.starts_with("--delete-")) {
        return deny_permanent("rsync --delete 会执行不可恢复删除");
    }

    if DISK_PATTERNS.iter().any(|re| re.is_match(&seg)) {
        return deny("F1", "磁盘级操作（dd/mkfs/erase/fdisk/parted）", None);
    }
    if SYSTEM_CMDS.iter().any(|re| re.is_match(&seg)) {
        return deny("F1", "系统属性或系统级进程操作", None);
    }
    if CRED_CMDS.iter().any(|re| re.is_match(&seg)) {
        return deny("F2", "凭证存储销毁（Keychain / GPG 私钥）", None);
    }
    if let Some((_, id, why)) = DESTROY_CMDS.iter().find(|(re, _, _)| re.is_match(&seg)) {
        return deny(id, *why, None);
    }
    if let Some((_, why)) = GIT_DESTROY.iter().find(|(re, _)| re.is_match(&seg)) {
        return deny("F3", *why, Some("删除单个分支用 git branch -d"));
    }
    let delete_verdict = eval_delete_segment(tokens, &seg, cwd);
    if !matches!(delete_verdict, Verdict::Allow) {
        return delete_verdict;
    }
    // Ask 档最后判定：具体危险（Deny/Recoverable）优先于审批
    if let Some((_, why)) = ASK_PATTERNS.iter().find(|(re, _)| re.is_match(&seg)) {
        return Verdict::Ask { reason: (*why).into() };
    }
    delete_verdict
}

fn command_is_dynamic(command: &str) -> bool {
    command.starts_with('$') || command.starts_with('`') || VAR_PATTERN.is_match(command)
}

fn deny_permanent(reason: impl Into<std::borrow::Cow<'static, str>>) -> Verdict {
    deny("F5", reason, Some("使用 delete tool 将目标移入系统废纸篓"))
}

fn eval_delete_segment(tokens: &[String], seg: &str, cwd: &str) -> Verdict {
    let cmd_idx = parse::command_index(tokens);
    let cmd = tokens.get(cmd_idx).map(|value| parse::command_name(value)).unwrap_or("");
    let is_delete = cmd == "trash" || (cmd == "find" && seg.contains(" -exec trash"));
    let is_move = MOVE_CMDS.contains(&cmd);
    if !is_delete && !is_move {
        return Verdict::Allow;
    }

    // trash 命令按可恢复降档（删除进回收站）：只拦 .git 与系统路径
    let recoverable = cmd == "trash";

    let targets: Vec<&str> = tokens.iter().skip(cmd_idx + 1).filter(|t| !t.starts_with('-')).map(String::as_str).collect();

    if targets.is_empty() && is_delete && (seg.contains("-r") || seg.contains("-f")) {
        return deny("F5", "递归/强制删除缺少可静态确定的目标路径", Some("明确写出完整目标路径后再执行"));
    }

    for target in targets {
        if VAR_PATTERN.is_match(target) {
            return deny("F5", format!("删除/移动目标含未求值变量 {target}，无法静态判定"), Some("先 echo 展开确认实际路径"));
        }
        if let Some(verdict) = protected_path_verdict(cmd, target, cwd, recoverable) {
            return verdict;
        }
    }

    if recoverable {
        return Verdict::Recoverable;
    }
    Verdict::Allow
}

fn protected_path_verdict(cmd: &str, target: &str, cwd: &str, recoverable: bool) -> Option<Verdict> {
    let hit = classify_path(target, cwd)?;
    if recoverable && hit.family == Family::Home {
        return None;
    }
    let rule = match hit.family {
        Family::Git => "F3",
        Family::Home | Family::Credential => "F2",
        Family::System => "F1",
    };
    Some(deny(rule, format!("{cmd} 的目标 {target} 命中保护路径 {}", hit.guard), Some("工作区内的具体子路径操作不受限，请缩小范围")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    System,
    Home,
    Credential,
    Git,
}

struct PathHit {
    family: Family,
    guard: std::borrow::Cow<'static, str>,
}

fn classify_path(target: &str, cwd: &str) -> Option<PathHit> {
    let norm = normalize_path(target, cwd);

    if EXEMPT_PREFIXES.iter().any(|p| norm == *p || norm.starts_with(&format!("{p}/"))) {
        return None;
    }
    if GIT_SEGMENT.is_match(&norm) {
        return Some(PathHit { family: Family::Git, guard: ".git".into() });
    }
    for guard in SYSTEM_PATHS {
        if *guard == "/" {
            if norm == "/" {
                return Some(PathHit { family: Family::System, guard: "/".into() });
            }
            continue;
        }
        if norm == *guard || norm.starts_with(&format!("{guard}/")) || guard.starts_with(&format!("{norm}/")) {
            return Some(PathHit { family: Family::System, guard: (*guard).into() });
        }
    }
    let home = dirs::home_dir()?;
    let home_str = home.to_string_lossy();
    if norm == home_str {
        return Some(PathHit { family: Family::Home, guard: home_str.to_string().into() });
    }
    for dot in home_credential_dot() {
        let guard = format!("{home_str}/{dot}");
        if norm == guard || norm.starts_with(&format!("{guard}/")) {
            return Some(PathHit { family: Family::Credential, guard: guard.into() });
        }
    }
    for top in home_top() {
        let guard = format!("{home_str}/{top}");
        if norm == guard {
            return Some(PathHit { family: Family::Home, guard: guard.into() });
        }
    }
    for rc in [".zshrc", ".bashrc", ".bash_profile", ".zprofile", ".profile"] {
        let guard = format!("{home_str}/{rc}");
        if norm == guard {
            return Some(PathHit { family: Family::Home, guard: guard.into() });
        }
    }
    // .config：拦整体删除，内容放行
    let guard = format!("{home_str}/.config");
    if norm == guard {
        return Some(PathHit { family: Family::Home, guard: guard.into() });
    }
    None
}

fn normalize_path(target: &str, cwd: &str) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
    // macOS /var、/tmp 是 /private/* 软链：cwd 先 canonicalize，否则临时区被误判为系统区
    let cwd_canon = std::fs::canonicalize(cwd).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| cwd.to_string());
    let mut s = if target == "~" {
        home.clone()
    } else if let Some(rest) = target.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else if target.starts_with('/') {
        std::fs::canonicalize(target).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| target.to_string())
    } else {
        format!("{cwd_canon}/{target}")
    };
    // 解析 . 与 .. 与多余斜杠
    let mut parts: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    s = format!("/{}", parts.join("/"));
    s
}

/// 路径守卫（write/edit/delete 的最终防线）。
pub fn guard_path(target: &str, cwd: &str) -> Verdict {
    match classify_path(target, cwd) {
        None => Verdict::Allow,
        Some(hit) => {
            let rule = match hit.family {
                Family::Git => "F3",
                Family::Home | Family::Credential => "F2",
                Family::System => "F1",
            };
            deny(rule, format!("路径 {target} 命中保护路径 {}", hit.guard), Some("工作区内的具体子路径操作不受限，请缩小范围"))
        }
    }
}

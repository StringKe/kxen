//! Shell 语法的最小词法层：只服务安全判定，不尝试执行或完整复刻 shell parser。

/// 在引号外按 shell 控制操作符切段，并保留引号内脚本文本为单个 token。
pub(super) fn segments(command: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else if ch == '\\' && delimiter == '"' {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            } else {
                word.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            ' ' | '\t' | '\r' => push_word(&mut tokens, &mut word),
            ';' | '|' | '&' | '\n' | '(' | ')' | '{' | '}' => {
                push_word(&mut tokens, &mut word);
                push_segment(&mut out, &mut tokens);
            }
            _ => word.push(ch),
        }
    }
    push_word(&mut tokens, &mut word);
    push_segment(&mut out, &mut tokens);
    out
}

fn push_word(tokens: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        tokens.push(std::mem::take(word));
    }
}

fn push_segment(out: &mut Vec<Vec<String>>, tokens: &mut Vec<String>) {
    if !tokens.is_empty() {
        out.push(std::mem::take(tokens));
    }
}

pub(super) fn command_name(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

pub(super) fn command_index(tokens: &[String]) -> usize {
    let mut i = skip_prefixes(tokens, 0);
    loop {
        let name = tokens.get(i).map(|v| command_name(v)).unwrap_or("");
        match name {
            "command" => {
                if tokens.get(i + 1).is_some_and(|v| matches!(v.as_str(), "-v" | "-V")) {
                    return i;
                }
                i = skip_options(tokens, i + 1, &[]);
            }
            "env" => {
                if env_split_requested(tokens, i) {
                    return i;
                }
                i = skip_options(tokens, i + 1, &["-u", "--unset", "-C", "--chdir"]);
            }
            "sudo" => i = skip_options(tokens, i + 1, &["-u", "--user", "-g", "--group", "-h", "--host", "-p", "--prompt", "-C"]),
            "doas" => i = skip_options(tokens, i + 1, &["-u"]),
            "exec" => i = skip_options(tokens, i + 1, &["-a"]),
            "nohup" => i = skip_options(tokens, i + 1, &[]),
            "nice" => i = skip_options(tokens, i + 1, &["-n", "--adjustment"]),
            "time" => i = skip_options(tokens, i + 1, &["-o", "--output", "-f", "--format"]),
            "timeout" => {
                i = skip_options(tokens, i + 1, &["-k", "--kill-after", "-s", "--signal"]);
                i = i.saturating_add(1);
            }
            "setsid" | "busybox" | "toybox" => i = skip_options(tokens, i + 1, &[]),
            _ => return i,
        }
        i = skip_prefixes(tokens, i);
    }
}

pub(super) fn env_split_requested(tokens: &[String], command: usize) -> bool {
    let mut i = command + 1;
    while let Some(token) = tokens.get(i) {
        if matches!(token.as_str(), "-S" | "--split-string") || token.starts_with("--split-string=") {
            return true;
        }
        if is_assignment(token) {
            i += 1;
        } else if matches!(token.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
            i += 2;
        } else if token.starts_with('-') {
            i += 1;
        } else {
            return false;
        }
    }
    false
}

fn skip_prefixes(tokens: &[String], mut i: usize) -> usize {
    while let Some(token) = tokens.get(i) {
        if is_assignment(token) || matches!(token.as_str(), "then" | "do" | "else" | "!") {
            i += 1;
        } else {
            break;
        }
    }
    i
}

fn skip_options(tokens: &[String], mut i: usize, takes_value: &[&str]) -> usize {
    while let Some(token) = tokens.get(i) {
        if token == "--" {
            return i + 1;
        }
        if is_assignment(token) {
            i += 1;
            continue;
        }
        if !token.starts_with('-') || token == "-" {
            break;
        }
        let option = token.split('=').next().unwrap_or(token);
        i += 1;
        if takes_value.contains(&option) && !token.contains('=') && token.len() == option.len() {
            i += 1;
        }
    }
    i
}

fn is_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else { return false };
    !name.is_empty() && !name.starts_with(|ch: char| ch.is_ascii_digit()) && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(super) fn nested_script(tokens: &[String], command: usize) -> Option<&str> {
    let options = tokens.get(command + 1..)?;
    options
        .windows(2)
        .find(|pair| {
            pair[0] == "--command"
                || (pair[0].starts_with('-') && !pair[0].starts_with("--") && pair[0].trim_start_matches('-').contains('c'))
        })
        .map(|pair| pair[1].as_str())
}

pub(super) fn xargs_command_index(tokens: &[String], command: usize) -> usize {
    skip_options(
        tokens,
        command + 1,
        &[
            "-a",
            "--arg-file",
            "-d",
            "--delimiter",
            "-E",
            "--eof",
            "-I",
            "--replace",
            "-J",
            "-L",
            "--max-lines",
            "-n",
            "--max-args",
            "-P",
            "--max-procs",
            "-R",
            "-S",
            "-s",
            "--max-chars",
            "--process-slot-var",
        ],
    )
}

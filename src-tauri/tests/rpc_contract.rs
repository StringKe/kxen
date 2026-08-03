//! 生产前端调用、后端 handler 与 request_schema 的静态闭环门禁。
//!
//! 动态拼 RPC 名会让静态对账失真，因此生产代码中的 `client.rpc` 第一参数必须是字符串字面量。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const HANDLER_FILES: &[&str] = &[
    "src/ws/rpc.rs",
    "src/ws/ops.rs",
    "src/ws/ops_knowledge.rs",
    "src/ws/ops_provider.rs",
    "src/ws/ops_workspace.rs",
    "src/ws/ops_mcp.rs",
    "src/ws/ops_recovery.rs",
    "src/ws/worktree_rpc.rs",
    "src/goal_rpc.rs",
];

#[test]
fn production_frontend_handlers_and_schemas_are_exactly_symmetric() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let handlers = handler_methods(manifest);
    let schemas = quoted_literals(&manifest.join("src/ws/request_schema/methods.rs"));
    let frontend = frontend_methods(&manifest.join("../src"));

    assert_sets_equal("handler <-> request_schema", &handlers, &schemas);
    assert_sets_equal("production frontend <-> handler", &frontend, &handlers);
}

fn handler_methods(manifest: &Path) -> BTreeSet<String> {
    let mut methods = BTreeSet::new();
    for relative in HANDLER_FILES {
        let path = manifest.join(relative);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        for line in text.lines() {
            let Some(arrow) = line.find("=>") else { continue };
            methods.extend(quoted_literals_from(&line[..arrow]).into_iter().filter(|method| !method.ends_with('.')));
        }
    }
    methods
}

fn frontend_methods(root: &Path) -> BTreeSet<String> {
    let mut files = Vec::new();
    visit_frontend(root, &mut files);
    let mut methods = BTreeSet::new();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let mut offset = 0;
        while let Some(found) = text[offset..].find("client.rpc") {
            let start = offset + found;
            let suffix = &text[start + "client.rpc".len()..];
            let open = suffix.find('(').unwrap_or_else(|| panic!("{}: client.rpc 缺少调用括号", path.display()));
            let argument = suffix[open + 1..].trim_start();
            let Some(quote @ ('\"' | '\'')) = argument.chars().next() else {
                panic!("{}: client.rpc 第一参数必须是字符串字面量", path.display());
            };
            let value = read_quoted(argument, quote).unwrap_or_else(|| panic!("{}: RPC 字符串未闭合", path.display()));
            methods.insert(value);
            offset = start + "client.rpc".len();
        }
    }
    methods
}

fn visit_frontend(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display())).flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_frontend(&path, out);
            continue;
        }
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let production = matches!(path.extension().and_then(|ext| ext.to_str()), Some("ts" | "tsx"))
            && !name.contains(".test.")
            && !name.contains(".spec.")
            && !name.ends_with(".d.ts");
        if production {
            out.push(path);
        }
    }
}

fn quoted_literals(path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    quoted_literals_from(&text)
}

fn quoted_literals_from(text: &str) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find('\"') {
        rest = &rest[start..];
        let Some(value) = read_quoted(rest, '\"') else { break };
        let consumed = value.len() + 2;
        values.insert(value);
        rest = &rest[consumed..];
    }
    values
}

fn read_quoted(text: &str, quote: char) -> Option<String> {
    let mut escaped = false;
    let mut value = String::new();
    for ch in text[quote.len_utf8()..].chars() {
        if escaped {
            value.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some(value);
        } else {
            value.push(ch);
        }
    }
    None
}

fn assert_sets_equal(label: &str, left: &BTreeSet<String>, right: &BTreeSet<String>) {
    let only_left: Vec<_> = left.difference(right).collect();
    let only_right: Vec<_> = right.difference(left).collect();
    assert!(only_left.is_empty() && only_right.is_empty(), "{label} 漂移\n只在左侧: {only_left:?}\n只在右侧: {only_right:?}");
}

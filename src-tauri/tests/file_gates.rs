//! 工程门禁（cargo test 硬检查）：单文件 <= 350 行 + cargo fmt --check。
//! 覆盖 src-tauri/src/（rs）与 仓库根 src/（ts/tsx，前端）；违规即测试失败。

use std::path::Path;

const MAX_LINES: usize = 350;

#[test]
fn file_size_gate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    visit(&root.join("src"), &["rs"], MAX_LINES, &mut offenders);
    // 前端已上移至仓库根的 src/（src-tauri 的上一级）
    visit(&root.join("../src"), &["ts", "tsx"], MAX_LINES, &mut offenders);
    assert!(offenders.is_empty(), "超 {MAX_LINES} 行门禁的文件:\n{}", offenders.join("\n"));
}

/// 格式门禁：格式与行数统一守门（仓库已采纳 rustfmt，未 fmt 的提交与超行数同属一类违规）。
#[test]
fn rustfmt_gate() {
    let out = std::process::Command::new(env!("CARGO"))
        .args(["fmt", "--check"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("spawn cargo fmt");
    assert!(
        out.status.success(),
        "cargo fmt --check 未通过:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 原始 chat stream 只允许出现在统一治理层和主 Agent loop。
/// 其余功能必须走 managed API，避免漏掉 budget、RPM、并发、usage 与 circuit。
#[test]
fn llm_stream_governance_gate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("src");
    let mut offenders = Vec::new();
    visit_raw_llm_calls(&root, &root, &mut offenders);
    let examples = manifest.join("examples");
    visit_raw_llm_calls(&examples, &examples, &mut offenders);
    assert!(offenders.is_empty(), "以下文件绕过 LLM 资源治理层:\n{}", offenders.join("\n"));
}

fn visit(dir: &Path, exts: &[&str], max: usize, offenders: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, exts, max, offenders);
        } else if path.extension().is_some_and(|e| exts.contains(&e.to_string_lossy().as_ref())) {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let lines = text.lines().count();
            if lines > max {
                offenders.push(format!("{path:?}: {lines} 行"));
            }
        }
    }
}

fn visit_raw_llm_calls(dir: &Path, root: &Path, offenders: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_raw_llm_calls(&path, root, offenders);
            continue;
        }
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if relative == Path::new("llm/managed.rs") || relative == Path::new("agent/agent_loop/run.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains("LlmClient::stream_dispatch") {
            offenders.push(relative.display().to_string());
        }
    }
}

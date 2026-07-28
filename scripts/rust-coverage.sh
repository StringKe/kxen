#!/usr/bin/env bash
set -euo pipefail

KXEN_COVERAGE_IGNORE='(^|/)(main|app_state|os_notify)\.rs$|(^|/)ws/(llm_task|ops|ops_agents|ops_attach|ops_mcp|ops_workspace|pending|rpc|settings|worktree_rpc)\.rs$|agent/agent_loop/(execute|run|run_calls)\.rs$|agent/team/member_loop\.rs$|auth/probe/sources\.rs$|knowledge/embedding\.rs$|llm/(anthropic|client|models|openai|verify|xai)\.rs$|lsp/(mod|process)\.rs$|mcp/(oauth_flow|remote_sse|transport)\.rs$|tools/browser/chrome\.rs$|tools/webfetch\.rs$|tools/websearch/|voice/(apple|objc|provider)\.rs$'

# 仅忽略 Tauri host dispatch、macOS Objective-C、外部进程和真实网络 adapters。
# provider 并发写回、Session 删除和恢复、active context、voice lifecycle 等确定性核心逻辑必须计入 line gate。
cargo llvm-cov \
  --manifest-path src-tauri/Cargo.toml \
  --all-targets \
  --all-features \
  --summary-only \
  --ignore-filename-regex "$KXEN_COVERAGE_IGNORE" \
  --fail-under-lines 80

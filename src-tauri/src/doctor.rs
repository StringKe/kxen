use kxen_app::auth::credential::AuthStore;
use kxen_app::auth::probe::RULES;
use kxen_app::core::paths;
use serde::Serialize;
use std::sync::Arc;

use crate::AppState;

#[derive(Debug, Serialize)]
pub struct DoctorEntry {
    pub provider: String,
    pub display: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub runtime: String,
    pub data_dir: String,
    pub config_dir: String,
    pub entries: Vec<DoctorEntry>,
    /// 子系统健康（MCP/LSP/MRM/event bus）：仅 RPC 路径填（需 AppState），reprobe 纯凭证路径为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemHealth>,
}

#[derive(Debug, Serialize)]
pub struct LspHealth {
    pub language: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct SystemHealth {
    pub mcp: Vec<kxen_app::mcp::ServerStatus>,
    pub lsp_root: String,
    pub lsp: Vec<LspHealth>,
    pub mrm_describe: String,
    pub mrm_dispatches: usize,
    pub bus_capacity: usize,
    pub bus_receivers: usize,
}

/// 子系统健康汇总：各 manager 现有 status/describe API 的只读拼装，不触发任何启动/连接动作。
pub async fn system_health(state: &Arc<AppState>) -> Result<SystemHealth, String> {
    let runtime = state.active_runtime()?;
    let mcp = runtime.mcp().status();
    let (lsp_root, lsp) = {
        let lsp = runtime.lsp();
        let root = lsp.root().to_string_lossy().into_owned();
        let entries = lsp.status().await.into_iter().map(|(language, status)| LspHealth { language, status }).collect();
        (root, entries)
    };
    let (mrm_describe, mrm_dispatches) = {
        let mrm = kxen_app::core::shared::read(&state.mrm).clone();
        (mrm.describe().await, mrm.history().await.len())
    };
    let (bus_capacity, bus_receivers) = state.bus.stats();
    Ok(SystemHealth { mcp, lsp_root, lsp, mrm_describe, mrm_dispatches, bus_capacity, bus_receivers })
}

/// 渲染当前 store 状态。探测只发生在启动后台任务（keychain 可阻塞），RPC 路径绝不触发 keychain。
/// 多账号：默认账号（官方导入）+ 命名账号各占一行。
pub fn doctor_report(store: &AuthStore) -> DoctorReport {
    let mut entries: Vec<DoctorEntry> = Vec::new();
    for rule in RULES {
        let (status, detail) = match store.get(rule.provider) {
            None => ("missing", "no credential found"),
            Some(c) if c.is_expired() => ("expired", "will refresh on next call"),
            Some(_) => ("ok", "credential present"),
        };
        entries.push(DoctorEntry {
            provider: rule.provider.to_string(),
            display: rule.display.to_string(),
            status: status.into(),
            detail: detail.into(),
        });
        // 命名账号行
        for key in kxen_account_keys(store, rule.provider) {
            let name = key.strip_prefix(&format!("{}:", rule.provider)).unwrap_or(&key);
            let (status, detail) = match store.get(&key) {
                Some(c) if c.is_expired() => ("expired", "will refresh on next call"),
                Some(_) => ("ok", "credential present"),
                None => ("missing", "no credential found"),
            };
            entries.push(DoctorEntry {
                provider: key.clone(),
                display: format!("{} · {}", rule.display, name),
                status: status.into(),
                detail: detail.into(),
            });
        }
    }
    DoctorReport {
        runtime: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: paths::data_dir().display().to_string(),
        config_dir: paths::config_dir().display().to_string(),
        entries,
        system: None,
    }
}

fn kxen_account_keys(store: &AuthStore, provider: &str) -> Vec<String> {
    kxen_app::auth::credential::accounts_of(store, provider).into_iter().filter(|k| k != provider).collect()
}

/// /doctor 发送路径拦截判定：命中即直出报告不起 run（llm_task 在 /compact 同款拦截位调用）
pub fn is_doctor_command(text: &str) -> bool {
    text.trim() == "/doctor"
}

/// 报告直出：凭证 + 子系统健康 -> markdown 落盘为 assistant 消息，不经 LLM（否则模型自由发挥，与菜单语义脱节）
pub async fn reply_with_report(
    state: &Arc<AppState>,
    sessions_dir: &std::path::Path,
    session_id: &str,
    message_id: Option<&str>,
) -> Result<(), String> {
    use kxen_app::core::session as ses;
    let store = state.auth_store.lock().map(|s| s.clone()).unwrap_or_default();
    let mut report = doctor_report(&store);
    report.system = system_health(state).await.ok();
    let mut msg = ses::new_message(session_id, ses::Role::Assistant, vec![ses::Part::Text { text: format_markdown(&report) }]);
    if let Some(message_id) = message_id {
        msg.id = message_id.to_string();
    }
    let result =
        if message_id.is_some() { ses::append_message_idempotent(sessions_dir, &msg) } else { ses::append_message(sessions_dir, &msg) };
    result.map(|_| ()).map_err(|e| format!("session append failed: {e}"))
}

/// 报告渲染为 markdown：/doctor 的会话内呈现（与 RPC 的结构化 JSON 共用同一数据源）
pub fn format_markdown(report: &DoctorReport) -> String {
    let mut out = format!(
        "## 环境自检\n\n- 版本：{}\n- 数据目录：`{}`\n- 配置目录：`{}`\n\n### 账号凭证\n\n| 账号 | 状态 |\n| --- | --- |\n",
        report.runtime, report.data_dir, report.config_dir
    );
    for e in &report.entries {
        let status = match e.status.as_str() {
            "ok" => "正常",
            "expired" => "已过期（下次调用自动刷新）",
            _ => "未配置",
        };
        out.push_str(&format!("| {} | {} |\n", e.display, status));
    }
    let Some(s) = &report.system else { return out };
    out.push_str("\n### MCP Servers\n\n");
    if s.mcp.is_empty() {
        out.push_str("（未配置）\n");
    } else {
        out.push_str("| Server | 状态 | Transport | 工具 | 资源 |\n| --- | --- | --- | --- | --- |\n");
        for m in &s.mcp {
            let status = match m.status.as_str() {
                "running" => "运行中",
                "down" => "不可用",
                "needs_auth" => "待授权",
                other => other,
            };
            out.push_str(&format!("| {} | {} | {} | {} | {} |\n", m.name, status, m.transport, m.tools, m.resources));
        }
    }
    out.push_str(&format!("\n### LSP\n\n- root：`{}`\n", s.lsp_root));
    if s.lsp.is_empty() {
        out.push_str("- 无已触发实例（懒启动：未触发 = 状态未知）\n");
    } else {
        out.push_str("\n| 语言 | 状态 |\n| --- | --- |\n");
        for l in &s.lsp {
            let status = if l.status == "running" { "运行中" } else { l.status.as_str() };
            out.push_str(&format!("| {} | {} |\n", l.language, status));
        }
    }
    // describe 是多行文本（每 provider 一行配额），代码块保住换行
    out.push_str(&format!("\n### MRM\n\n```\n{}\n```\n\n- 累计派发：{}\n", s.mrm_describe, s.mrm_dispatches));
    // 0 订阅 = 事件全在丢（event.rs 判定的异常态），报告必须显性标出
    let bus_note = if s.bus_receivers == 0 { "（异常：无订阅者，事件全在丢）" } else { "" };
    out.push_str(&format!("\n### Event Bus\n\n- 容量：{}\n- 活跃订阅：{}{}\n", s.bus_capacity, s.bus_receivers, bus_note));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_system() -> DoctorReport {
        DoctorReport {
            runtime: "0.1.0".into(),
            data_dir: "/tmp/kxen-data".into(),
            config_dir: "/tmp/kxen-config".into(),
            entries: vec![
                DoctorEntry { provider: "kimi".into(), display: "Kimi for Coding".into(), status: "ok".into(), detail: String::new() },
                DoctorEntry { provider: "xai:work".into(), display: "xAI work".into(), status: "expired".into(), detail: String::new() },
                DoctorEntry { provider: "anthropic".into(), display: "Anthropic".into(), status: "missing".into(), detail: String::new() },
            ],
            system: Some(SystemHealth {
                mcp: vec![kxen_app::mcp::ServerStatus {
                    name: "fs".into(),
                    status: "running".into(),
                    transport: "stdio".into(),
                    url: None,
                    tools: 5,
                    resources: 2,
                    prompts: vec![],
                    last_auth_error: None,
                }],
                lsp_root: "/tmp/proj".into(),
                lsp: vec![LspHealth { language: "rust".into(), status: "running".into() }],
                mrm_describe: "global limit: 8".into(),
                mrm_dispatches: 3,
                bus_capacity: 256,
                bus_receivers: 2,
            }),
        }
    }

    #[test]
    fn doctor_command_intercepts_exact_slash() {
        // 只有精确 /doctor 才拦截：带参数、前缀相似、普通文本都必须放行给正常路径
        assert!(is_doctor_command("/doctor"));
        assert!(is_doctor_command("  /doctor  "));
        assert!(!is_doctor_command("/doctor extra"));
        assert!(!is_doctor_command("/doctorx"));
        assert!(!is_doctor_command("hello"));
    }

    #[test]
    fn markdown_covers_all_sections() {
        let md = format_markdown(&report_with_system());
        for section in ["## 环境自检", "### 账号凭证", "### MCP Servers", "### LSP", "### MRM", "### Event Bus"] {
            assert!(md.contains(section), "缺段落 {section}:\n{md}");
        }
        assert!(md.contains("/tmp/kxen-data") && md.contains("/tmp/kxen-config"));
        // 账号表三态行
        assert!(md.contains("| Kimi for Coding | 正常 |"));
        assert!(md.contains("| xAI work | 已过期（下次调用自动刷新） |"));
        assert!(md.contains("| Anthropic | 未配置 |"));
        // 子系统行
        assert!(md.contains("| fs | 运行中 | stdio | 5 | 2 |"));
        assert!(md.contains("| rust | 运行中 |"));
        assert!(md.contains("global limit: 8") && md.contains("累计派发：3"));
        assert!(md.contains("容量：256") && md.contains("活跃订阅：2"));
    }

    #[test]
    fn markdown_flags_bus_without_subscribers() {
        let mut report = report_with_system();
        report.system.as_mut().unwrap().bus_receivers = 0;
        let md = format_markdown(&report);
        assert!(md.contains("活跃订阅：0（异常"), "0 订阅未标异常:\n{md}");
    }

    #[test]
    fn markdown_omits_system_when_absent() {
        let mut report = report_with_system();
        report.system = None;
        let md = format_markdown(&report);
        // reprobe 纯凭证路径无子系统数据，渲染不得留空标题
        assert!(md.contains("### 账号凭证"));
        assert!(!md.contains("### MCP Servers") && !md.contains("### Event Bus"));
    }
}

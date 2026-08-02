//! agents.stop / agents.dismiss：按名停止/移除子代理。

use serde_json::{Value, json};
use std::sync::Arc;

use crate::AppState;

/// 按 agent name 停止：teammate 走 team shutdown（cancel token + 状态落盘），
/// subagent/workflow 走活动注册表的取消句柄；名单里不存在的 name 返回 false。
pub(super) async fn agents_stop(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
    let agents = state.agents.list(session_id);
    let Some(agent) = agents.iter().find(|a| a.name == name) else {
        return Ok(json!(false));
    };
    let stopped = match agent.kind {
        kxen_app::agent::activity::AgentKind::Teammate => {
            state.team.lead_action(session_id, &json!({ "action": "shutdown", "name": name })).await.is_ok()
        }
        _ => state.agents.cancel(session_id, name),
    };
    Ok(json!(stopped))
}

/// 按 agent name 移除终态条目：chip 的关闭出口；非终态或不存在返回 false（要停走 agents.stop）。
pub(super) async fn agents_dismiss(params: &Value, state: &Arc<AppState>) -> Result<Value, String> {
    let session_id = params.get("session_id").and_then(Value::as_str).ok_or("missing session_id")?;
    let name = params.get("name").and_then(Value::as_str).ok_or("missing name")?;
    Ok(json!(state.agents.dismiss(session_id, name)))
}

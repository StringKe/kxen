//! session_start hook 必须在 session.create 返回 id 后异步运行。
//! 否则 session-scoped Ask 在前端能订阅 `session:<id>` 前就阻塞 RPC。

pub(super) fn spawn(
    hooks: std::sync::Arc<kxen_app::tools::hooks::HookRunner>,
    approvals: std::sync::Arc<kxen_app::agent::approval::ApprovalBroker>,
    bus: kxen_app::core::event::EventBus,
    session_id: String,
    directory: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let payload = serde_json::json!({ "id": session_id, "directory": directory });
        let approval = kxen_app::tools::exec::ApprovalCtx::new(Some(approvals.as_ref()), Some(&bus), None, Some(session_id.as_str()));
        if let Err(error) = hooks.run_named_with_approval("session_start", &session_id, &payload, approval.as_ref()).await {
            tracing::warn!(%error, "session_start hook failed");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_returns_before_approval_and_pending_is_session_recoverable() {
        let config: kxen_app::core::config::Config = toml::from_str(
            r#"
[[hooks.session_start]]
command = "git push --force origin main"
"#,
        )
        .unwrap();
        let hooks = std::sync::Arc::new(kxen_app::tools::hooks::HookRunner::from_config(&config, std::path::Path::new("/tmp")));
        let broker = std::sync::Arc::new(kxen_app::agent::approval::ApprovalBroker::with_timeout(std::time::Duration::from_secs(5)));
        let bus = kxen_app::core::event::EventBus::new(8);

        let task = spawn(hooks, broker.clone(), bus, "session-new".into(), "/tmp".into());
        assert!(!task.is_finished(), "session.create 可先返回，hook 在后台等待审批");
        let pending = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(approval) = broker.list_pending(Some("session-new")).into_iter().next() {
                    break approval;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Session pending 快照应可恢复 hook approval");
        assert_eq!(pending.command, "git push --force origin main");
        assert!(broker.list_pending(None).is_empty(), "session hook 不能复制到全局恢复面");
        assert!(broker.respond(&pending.id, false));
        tokio::time::timeout(std::time::Duration::from_secs(1), task).await.unwrap().unwrap();
    }
}

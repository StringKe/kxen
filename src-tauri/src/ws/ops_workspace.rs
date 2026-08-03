//! workspace 域 RPC：工作看板卡片数据（仿 ops_provider 分文件模式）。

use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Manager};

use crate::AppState;

pub(super) const METHODS: &[&str] = &["workspaces.overview"];

pub(super) async fn handle(method: &str, _params: &Value, app: &AppHandle) -> Result<Value, String> {
    match method {
        "workspaces.overview" => {
            let state = app.state::<Arc<AppState>>();
            let sessions = kxen_app::core::session::list_checked(&kxen_app::core::paths::sessions_dir())
                .map_err(|error| format!("session catalog unavailable: {error}"))?;
            let running: std::collections::HashSet<String> = kxen_app::core::shared::lock(&state.active_runs).keys().cloned().collect();
            let workspaces = kxen_app::core::workspace::list(&kxen_app::core::paths::data_dir()).map_err(|error| error.to_string())?;
            // queue/cron 都是内存快照，一次锁取出
            let queued = state.pending_messages.counts();
            let cron = kxen_app::core::schedule::list()?;
            // goal 一次全量读盘后按会话归属分配：逐 session focus_for 会把磁盘读放大 N 倍
            let goals = kxen_app::core::goal::Goal::list_checked(&kxen_app::core::paths::goals_dir()).map_err(|error| error.to_string())?;
            let worktrees = gather_worktrees(&workspaces).await?;
            // 聚合内 dirty_count 是同步 git spawn（每 workspace 一次）：移出 async worker，不卡运行时
            let cards = tauri::async_runtime::spawn_blocking(move || {
                kxen_app::core::workspace::overview(workspaces, &sessions, &running, &queued, &goals, &cron, &worktrees)
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok(json!(cards))
        }
        _ => Err(format!("unknown method: {method}")),
    }
}

/// 逐 workspace 采集 kxen 隔离树摘要（name/branch/dirty）。
/// 成本门：先查 `<ws>/.kxen/worktrees` 目录存在再 spawn git——没建过隔离树的 workspace 零进程开销；
/// 多 workspace 并发采集（JoinSet）：最近列表可达 20 项，串行 spawn 会把尾延迟放大到秒级。
async fn gather_worktrees(
    workspaces: &[kxen_app::core::workspace::Workspace],
) -> Result<std::collections::HashMap<String, Vec<kxen_app::core::workspace::WorktreeDigest>>, String> {
    let mut set = tokio::task::JoinSet::new();
    for w in workspaces {
        let root = std::path::PathBuf::from(&w.path);
        let worktree_dir = root.join(".kxen").join("worktrees");
        match std::fs::metadata(&worktree_dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(format!("worktree store is not a directory: {}", worktree_dir.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("inspect worktree store {}: {error}", worktree_dir.display())),
        }
        let key = w.path.clone();
        set.spawn(async move {
            let list =
                kxen_app::tools::worktree::list(&root).await.map_err(|error| format!("list worktrees for {}: {error}", root.display()))?;
            let mut out = Vec::with_capacity(list.len());
            for t in list {
                // 单棵树 status 失败以 dirty=None 明确表示 UNKNOWN，不把整张 workspace 卡片降成失败。
                let dirty = kxen_app::tools::worktree::status(&t.path).await.ok().map(|v| v.len());
                out.push(kxen_app::core::workspace::WorktreeDigest {
                    name: t.name,
                    branch: t.branch,
                    path: t.path.to_string_lossy().into_owned(),
                    dirty,
                    // 绑定计数由 overview 聚合填充：采集层拿不到会话列表
                    sessions: 0,
                    running: 0,
                });
            }
            Ok::<_, String>((key, out))
        });
    }
    let mut map = std::collections::HashMap::new();
    while let Some(result) = set.join_next().await {
        let (path, trees) = result.map_err(|error| format!("worktree inspection task failed: {error}"))??;
        map.insert(path, trees);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worktree_list_failure_is_not_reported_as_empty() {
        let root = std::env::temp_dir().join(format!("kxen-workspace-worktree-error-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".kxen/worktrees")).unwrap();
        let workspaces = vec![kxen_app::core::workspace::Workspace { path: root.to_string_lossy().into_owned(), last_used: 0 }];

        let error = gather_worktrees(&workspaces).await.expect_err("non-git workspace with a worktree store must return the list error");
        assert!(error.contains("list worktrees"));
        assert!(error.contains(&root.to_string_lossy().to_string()));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn missing_worktree_store_is_a_valid_empty_result() {
        let root = std::env::temp_dir().join(format!("kxen-workspace-no-worktrees-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.to_string_lossy().into_owned();
        let workspaces = vec![kxen_app::core::workspace::Workspace { path: path.clone(), last_used: 0 }];

        let gathered = gather_worktrees(&workspaces).await.unwrap();
        assert!(!gathered.contains_key(&path));
        std::fs::remove_dir_all(root).ok();
    }
}

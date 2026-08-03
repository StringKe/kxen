//! AppState：全局服务与运行时共享状态。

use std::sync::{Arc, Mutex};

pub struct AppState {
    /// 单实例文件锁：所有 data_dir JSON store 的进程内事务假设由此提升为跨进程安全。
    _instance_lock: std::fs::File,
    /// 共享句柄（Arc 内层不变）：TeamManager SpawnDeps 持同一把锁，凭证探测/刷新后操作点可见
    pub auth_store: Arc<Mutex<kxen_app::auth::credential::AuthStore>>,
    /// ws 服务端口（serve 成功后写入，ws_port command 用）
    pub(crate) ws_port: Mutex<u16>,
    /// ws 握手 token（启动时 /dev/urandom 生成，ws_port command 一并发给前端）
    pub ws_token: String,
    pub bus: kxen_app::core::event::EventBus,
    pub registry: std::sync::Arc<kxen_app::tools::task::TaskRegistry>,
    /// 角色路由可热更新（设置页改角色 -> 重建换 Arc）；与 SpawnDeps 共享同一 RwLock 句柄
    pub mrm: std::sync::Arc<std::sync::RwLock<std::sync::Arc<kxen_app::llm::mrm::ModelResourceManager>>>,
    pub extras: std::sync::Arc<kxen_app::agent::agent_loop::SessionExtrasRegistry>,
    /// Ask 档审批 broker（exec 高危命令的用户决定路由）
    pub approvals: std::sync::Arc<kxen_app::agent::approval::ApprovalBroker>,
    /// MCP、LSP 和 Hooks 按规范化 Workspace 根隔离，后台 Session 不跟随前台目录漂移。
    pub workspace_runtimes: std::sync::Arc<kxen_app::workspace_runtime::WorkspaceRuntimeRegistry>,
    pub team: std::sync::Arc<kxen_app::agent::team::TeamManager>,
    pub agents: std::sync::Arc<kxen_app::agent::activity::AgentRegistry>,
    /// session_id -> 进行中 run 的取消令牌（session.abort 用；run 结束自行移除）
    pub active_runs: std::sync::Mutex<std::collections::HashMap<String, kxen_app::agent::cancel::CancelToken>>,
    /// session 排队消息（落盘持久化：崩溃重启可恢复续跑；run 结束按序接续，防并发 run 交叉写历史）
    pub pending_messages: std::sync::Arc<kxen_app::core::pending_queue::PendingQueues>,
    /// session_id -> 已知 token 下界与 UNKNOWN 调用数（状态栏用量段）
    pub session_tokens: Arc<std::sync::Mutex<std::collections::HashMap<String, kxen_app::core::usage::SessionUsage>>>,
    /// session_id -> 最近一次 run 的 input tokens（ctx 占用近似值，进度条数据源）
    pub session_last_input: std::sync::Mutex<std::collections::HashMap<String, u64>>,
    /// 状态栏显隐段（启动时从 config 读；设置页改后重建）
    pub statusline_items: std::sync::Mutex<Vec<String>>,
    /// git 分支 5s 缓存按 Workspace 隔离，跨会话切换不能复用别的项目分支。
    pub git_cache: std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, (std::time::Instant, String)>>,
    pub workdir: std::sync::Arc<std::path::Path>,
    /// 当前活跃 workspace（多项目目录，可切换；初始 = workdir）
    pub active_workspace: std::sync::RwLock<std::path::PathBuf>,
    /// session_id -> agent 改动快照（改动面板数据源；run 间共享，随 app 存活——不落盘的 WHY 见 snapshot 模块 doc；
    /// session.delete 经 snapshot::drop_session 摘除，不泄漏）
    pub session_snapshots: std::sync::Mutex<std::collections::HashMap<String, kxen_app::tools::snapshot::SnapshotStore>>,
    /// session_id -> 最近一轮 run 的 involved 文件（injection_preview 的真实 glob 命中数据源）
    pub session_involved: std::sync::Mutex<std::collections::HashMap<String, Vec<std::path::PathBuf>>>,
    /// 通知环形缓冲（teammate/cron/系统事件，顶栏通知中心数据源，50 条）
    pub notifications: std::sync::Mutex<std::collections::VecDeque<kxen_app::core::notifications::Notice>>,
    /// 前台聚焦会话（OS 通知只发非前台会话的完成事件）
    pub foreground_session: std::sync::RwLock<String>,
    /// 原生对话框附件授权清单（选择即授权；context 边界守卫与 read_attachment 的唯一放行依据）
    pub picked_files: kxen_app::core::attachment::PickedFiles,
}

impl AppState {
    pub(crate) fn new() -> Result<Self, String> {
        let data_dir = kxen_app::core::paths::data_dir();
        ensure_private_data_dir(&data_dir)?;
        let instance_lock = acquire_instance_lock(&data_dir)?;
        let path = kxen_app::core::paths::auth_file();
        let config_path = kxen_app::core::paths::config_dir().join("config.toml");
        crate::ws::recover_custom_provider_transaction(&config_path, &path)?;
        // 共享句柄：与 TeamManager SpawnDeps 同一把锁，后台探测写入的凭证两边即时可见；
        // 登记回写后 run 内刷新（ctx.store 是克隆快照）也即时收敛到各克隆点（auth::shared_store）
        let store = Arc::new(Mutex::new(
            kxen_app::auth::credential::read_auth_file(&path).map_err(|error| format!("auth store load failed: {error}"))?,
        ));
        kxen_app::auth::shared_store::register_shared_store(&store);
        let config = load_app_config(&config_path)?;
        let statusline_items = config.statusline.items.clone();
        let registry = std::sync::Arc::new(kxen_app::tools::task::TaskRegistry::new());
        let extras = std::sync::Arc::new(kxen_app::agent::agent_loop::SessionExtrasRegistry::default());
        let mrm = std::sync::Arc::new(std::sync::RwLock::new(std::sync::Arc::new(kxen_app::llm::mrm::ModelResourceManager::new(config))));
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        let workdir: std::sync::Arc<std::path::Path> = std::sync::Arc::from(initial_workdir(&cwd, dirs::home_dir()));
        let bus = kxen_app::core::event::EventBus::default();
        let agents = std::sync::Arc::new(kxen_app::agent::activity::AgentRegistry::default());
        let approvals = std::sync::Arc::new(kxen_app::agent::approval::production_broker(bus.clone()));
        let workspace_runtimes = std::sync::Arc::new(kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::with_mcp_execution_approval(
            approvals.clone(),
            bus.clone(),
            mrm.clone(),
        ));
        workspace_runtimes.runtime(&workdir)?;
        // P0-2：team relay 与 AppState 共享同一队列实例（teammate 报告入队 = 用户消息同路续跑）
        let pending_messages =
            std::sync::Arc::new(kxen_app::core::pending_queue::PendingQueues::new(kxen_app::core::paths::sessions_dir()));
        let mut loaded_usage = kxen_app::core::usage::load().map_err(|error| format!("session usage load failed: {error}"))?;
        for warning in kxen_app::core::usage::reconcile_pending_goal_charges(&mut loaded_usage)
            .map_err(|error| format!("pending usage reconciliation failed: {error}"))?
        {
            tracing::warn!(%warning, "pending usage durability repaired during startup");
        }
        for warning in kxen_app::core::usage::reconcile_provider_attempts(&mut loaded_usage)
            .map_err(|error| format!("Provider attempt reconciliation failed: {error}"))?
        {
            tracing::warn!(%warning, "Provider attempt durability repaired during startup");
        }
        for warning in kxen_app::core::goal::Goal::reconcile_completion_attempts(&kxen_app::core::paths::goals_dir())
            .map_err(|error| format!("completion attempt reconciliation failed: {error}"))?
        {
            tracing::warn!(%warning, "completion attempt recovered during startup");
        }
        let knowledge_metering_operations = kxen_app::knowledge::consolidate::pending_metering_operation_ids()
            .map_err(|error| format!("Knowledge metering recovery scan failed: {error}"))?;
        for warning in kxen_app::core::usage::compact_closed_metering_receipts_preserving(&mut loaded_usage, &knowledge_metering_operations)
            .map_err(|error| format!("metering receipt compaction failed: {error}"))?
        {
            tracing::warn!(%warning, "metering receipt compaction deferred during startup");
        }
        let session_tokens = Arc::new(std::sync::Mutex::new(loaded_usage));
        let team = kxen_app::agent::team::TeamManager::new(
            kxen_app::core::paths::data_dir().join("teams"),
            kxen_app::agent::team::SpawnDeps {
                registry: registry.clone(),
                fallback_workdir: workdir.clone(),
                store: store.clone(),
                mrm: mrm.clone(),
                runtimes: workspace_runtimes.clone(),
                extras: extras.clone(),
                agents: agents.clone(),
                approvals: Some(approvals.clone()),
                session_usage: session_tokens.clone(),
            },
            bus.clone(),
            kxen_app::core::paths::sessions_dir(),
            Some(pending_messages.clone()),
        );
        Ok(Self {
            _instance_lock: instance_lock,
            auth_store: store,
            ws_port: Mutex::new(0),
            ws_token: crate::ws::gen_ws_token().map_err(|error| format!("generate websocket token: {error}"))?,
            bus,
            registry,
            extras,
            approvals,
            workspace_runtimes,
            team,
            agents,
            active_runs: std::sync::Mutex::new(std::collections::HashMap::new()),
            pending_messages,
            mrm,
            session_tokens,
            session_last_input: std::sync::Mutex::new(std::collections::HashMap::new()),
            statusline_items: std::sync::Mutex::new(statusline_items),
            git_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            active_workspace: std::sync::RwLock::new(workdir.to_path_buf()),
            session_snapshots: std::sync::Mutex::new(std::collections::HashMap::new()),
            session_involved: std::sync::Mutex::new(std::collections::HashMap::new()),
            notifications: std::sync::Mutex::new(
                kxen_app::core::notifications::load().map_err(|error| format!("notifications load failed: {error}"))?,
            ),
            foreground_session: std::sync::RwLock::new(String::new()),
            picked_files: kxen_app::core::attachment::PickedFiles::default(),
            workdir,
        })
    }

    /// 主会话 extras 取口：按 session 隔离，跨 send_message 存续。
    pub fn extras_for(&self, session_id: &str) -> std::sync::Arc<kxen_app::agent::agent_loop::SessionExtras> {
        self.extras.extras_for(session_id)
    }

    /// 会话销毁时清理该 Session 的运行期 extras。
    pub fn drop_extras(&self, session_id: &str) {
        self.extras.drop_extras(session_id);
    }

    pub fn active_runtime(&self) -> Result<std::sync::Arc<kxen_app::workspace_runtime::WorkspaceRuntime>, String> {
        let root = self.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string())?.clone();
        self.workspace_runtimes.runtime(&root)
    }

    /// Settings 与启动期状态查询必须等首次 MCP load 完成，不能把尚未 ready 的空 manager
    /// 误报为“未配置”。项目 stdio 审批可在等待期间由全局审批面应答。
    pub async fn ready_active_runtime(&self) -> Result<std::sync::Arc<kxen_app::workspace_runtime::WorkspaceRuntime>, String> {
        let root = self.active_workspace.read().map_err(|_| "workspace lock poisoned".to_string())?.clone();
        self.workspace_runtimes.ready(&root).await
    }

    pub fn runtime_for_session(&self, session_id: &str) -> Result<std::sync::Arc<kxen_app::workspace_runtime::WorkspaceRuntime>, String> {
        let meta = kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), session_id)
            .map_err(|e| format!("session {session_id}: {e}"))?;
        self.workspace_runtimes.runtime(std::path::Path::new(&meta.directory))
    }

    pub async fn ready_runtime_for_session(
        &self,
        session_id: &str,
    ) -> Result<std::sync::Arc<kxen_app::workspace_runtime::WorkspaceRuntime>, String> {
        let meta = kxen_app::core::session::load_meta(&kxen_app::core::paths::sessions_dir(), session_id)
            .map_err(|e| format!("session {session_id}: {e}"))?;
        self.workspace_runtimes.ready(std::path::Path::new(&meta.directory)).await
    }
}

fn ensure_private_data_dir(path: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|error| format!("create app data directory {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("secure app data directory {}: {error}", path.display()))?;
    }
    Ok(())
}

fn acquire_instance_lock(data_dir: &std::path::Path) -> Result<std::fs::File, String> {
    let path = data_dir.join("instance.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("open app instance lock {}: {error}", path.display()))?;
    file.try_lock().map_err(|error| format!("another kxen instance is using {}: {error}", data_dir.display()))?;
    Ok(file)
}

fn load_app_config(path: &std::path::Path) -> Result<kxen_app::core::config::Config, String> {
    kxen_app::core::config::Config::load(path, None).map_err(|error| format!("app config {}: {error}", path.display()))
}

/// 初始 workspace 根（纯函数，直接可测）：Finder 启动 .app 时进程 cwd 恒为 `/`，
/// 以之为 workspace 根会让 path_policy 的 starts_with(workspace) 边界全盘失效。
/// cwd 为根目录或不可写时回退 home；home 不可得（极端环境）兜底保留 cwd，不引入第三选择。
fn initial_workdir(cwd: &std::path::Path, home: Option<std::path::PathBuf>) -> std::path::PathBuf {
    if cwd != std::path::Path::new("/") && dir_writable(cwd) {
        return cwd.to_path_buf();
    }
    home.unwrap_or_else(|| cwd.to_path_buf())
}

/// 权限位只读判定：kxen 不以 root 运行（root 下位判断失真），元数据读取失败按不可写处理。
fn dir_writable(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir() && !m.permissions().readonly()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-workdir-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn initial_workdir_prefers_writable_cwd() {
        let dir = tmp_dir("ok");
        let home = std::path::PathBuf::from("/home/fallback");
        assert_eq!(initial_workdir(&dir, Some(home)), dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn initial_workdir_root_falls_back_to_home() {
        let home = std::path::PathBuf::from("/home/fallback");
        assert_eq!(initial_workdir(std::path::Path::new("/"), Some(home.clone())), home);
        // home 不可得时保留 cwd（最差兜底）
        assert_eq!(initial_workdir(std::path::Path::new("/"), None), std::path::PathBuf::from("/"));
    }

    #[test]
    fn initial_workdir_unwritable_falls_back_to_home() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_dir("ro");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let home = std::path::PathBuf::from("/home/fallback");
        assert_eq!(initial_workdir(&dir, Some(home.clone())), home);
        // 恢复可写才能清理
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_app_config_fails_closed_with_path() {
        let dir = std::env::temp_dir().join(format!("kxen-app-config-invalid-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create fixture root");
        let path = dir.join("config.toml");
        std::fs::write(&path, "broken = [toml").expect("write invalid config");

        let error = load_app_config(&path).expect_err("invalid startup config must fail");
        assert!(error.contains(&path.display().to_string()), "startup error must identify config path: {error}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn app_data_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kxen-private-data-{}", uuid::Uuid::new_v4()));
        ensure_private_data_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn app_instance_lock_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("kxen-instance-lock-{}", uuid::Uuid::new_v4()));
        ensure_private_data_dir(&dir).unwrap();
        let first = acquire_instance_lock(&dir).unwrap();
        assert!(acquire_instance_lock(&dir).is_err());
        drop(first);
        assert!(acquire_instance_lock(&dir).is_ok());
        std::fs::remove_dir_all(dir).ok();
    }
}

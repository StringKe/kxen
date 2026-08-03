//! 项目信任门：未信任 workspace 的项目知识（.agents rules/notes）只索引不进全文。
//! 决定持久化在 data_dir/trusted.json；审批走 ApprovalBroker（与 exec Ask 同一通道）。

use std::path::{Path, PathBuf};

pub type TrustCallback = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;

const TRUST_REASON: &str = "信任此项目？项目知识与配置将注入模型上下文；项目 hooks 可能执行本机代码。项目 .mcp.json 的 stdio 进程仍需随后按完整 command/args/cwd/env keys 独立审批，本次信任不会直接批准其执行";

fn store_file() -> PathBuf {
    // 测试隔离：环境变量覆盖（各测试模块用 Once 设同一值，写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_TRUST_FILE") {
        return PathBuf::from(p);
    }
    crate::core::paths::data_dir().join("trusted.json")
}

fn load_from(file: &Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("read {}: {error}", file.display())),
    };
    serde_json::from_str(&text).map_err(|error| format!("parse {}: {error}", file.display()))
}

fn trust_into(file: &Path, workdir: &Path) -> Result<Option<String>, String> {
    // 读-改-写竞态防护：并发 trust 会互相覆盖丢失条目（并行测试抓出来的真 bug）
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = crate::core::shared::lock(&WRITE_LOCK);
    let mut list = load_from(file)?;
    let w = workdir.to_string_lossy().into_owned();
    if !list.contains(&w) {
        list.push(w);
        // 原子写（tmp+rename）：非原子写在并发 load_from 下会读到半截文件
        return write_atomic(file, &list);
    }
    Ok(None)
}

pub fn load() -> Result<Vec<String>, String> {
    load_from(&store_file())
}

pub fn is_trusted(workdir: &Path) -> bool {
    let w = workdir.to_string_lossy();
    let list = match load() {
        Ok(list) => list,
        Err(error) => {
            tracing::error!(%error, "workspace trust store unavailable");
            return false;
        }
    };
    list.iter().any(|p| {
        if p == &w {
            return true;
        }
        // 子路径继承仅限 <repo>/.kxen/worktrees/<name>：worktree 是同一项目副本，
        // 精确匹配会让 worktree 会话退回未信任（custom role/skill/command 全失效）；
        // 不收宽到任意子目录——子目录可能是拖进项目的不可信第三方代码
        workdir.starts_with(Path::new(p).join(".kxen/worktrees"))
    })
}

pub fn trust(workdir: &Path) -> Result<Option<String>, String> {
    trust_into(&store_file(), workdir)
}

fn write_atomic(file: &Path, list: &[String]) -> Result<Option<String>, String> {
    use std::io::Write;
    let parent = file.parent().ok_or_else(|| format!("path has no parent: {}", file.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let tmp = file.with_extension("json.tmp");
    let text = serde_json::to_vec_pretty(list).map_err(|error| format!("serialize trust store: {error}"))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    output.write_all(&text).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    output.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(output);
    std::fs::rename(&tmp, file).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", file.display())
    })?;
    #[cfg(unix)]
    if let Err(error) = sync_trust_directory(parent) {
        return Ok(Some(format!("trust store is visible but directory sync failed for {}: {error}", parent.display())));
    }
    Ok(None)
}

#[cfg(unix)]
fn sync_trust_directory(parent: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_TRUST_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected trust directory sync failure"));
    }
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_TRUST_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 项目内存在需要信任决策的内容（知识树、项目级 hooks 配置或 MCP 服务器清单）。
pub fn needs_gate(workdir: &Path) -> bool {
    // .mcp.json 的 server 命令会在信任后由 MCP 加载执行，未信任前同样要过门
    workdir.join(".agents").is_dir() || workdir.join(".kxen/config.toml").is_file() || workdir.join(".mcp.json").is_file()
}

/// 项目 hooks 按信任门换入换出：已信任合并项目 .kxen/config.toml 重载，未信任回用户级。
pub fn reload_hooks_for_workspace(workdir: &Path, hooks: &crate::tools::hooks::HookRunner) {
    let project_cfg = if is_trusted(workdir) { Some(workdir.join(".kxen/config.toml")) } else { None };
    match crate::core::config::Config::load(&crate::core::paths::config_dir().join("config.toml"), project_cfg.as_deref()) {
        Ok(merged) => hooks.reload(&merged),
        Err(error) => {
            tracing::error!(%error, workspace = %workdir.display(), "hook config reload rejected; keeping current hooks");
        }
    }
}

/// workspace 切换后的信任门：未信任且含知识/项目配置 -> 后台审批（不阻塞切换）。
pub fn gate_async(
    workdir: &Path,
    broker: &std::sync::Arc<crate::agent::approval::ApprovalBroker>,
    bus: &crate::core::event::EventBus,
    on_trusted: Option<TrustCallback>,
) {
    if !needs_gate(workdir) || is_trusted(workdir) {
        return;
    }
    let broker = broker.clone();
    let bus = bus.clone();
    let dir = workdir.to_path_buf();
    tokio::spawn(async move {
        // 无会话归属（workspace 级审批）：register("") 记空归属，cancel_session 不误伤，决定不落盘
        let (id, rx) = broker.register("", &dir.display().to_string(), TRUST_REASON);
        bus.publish(crate::core::event::Event::LlmDelta(serde_json::json!({
            "kind": "approval",
            "approval_id": id,
            "command": dir.display().to_string(),
            "reason": TRUST_REASON,
        })));
        let outcome = broker.wait(&id, rx, None).await;
        if matches!(outcome, crate::agent::approval::ApprovalOutcome::Allow) {
            match trust(&dir) {
                Ok(warning) => {
                    if let Some(callback) = on_trusted {
                        callback(&dir);
                    }
                    let message = match warning {
                        Some(warning) => format!("已信任项目 {}，持久化已可见但需关注：{warning}", dir.display()),
                        None => format!("已信任项目 {}", dir.display()),
                    };
                    bus.publish(crate::core::event::Event::notify(message, None));
                }
                Err(error) => {
                    tracing::error!(%error, workspace = %dir.display(), "workspace trust save failed");
                    bus.publish(crate::core::event::Event::notify(format!("项目信任保存失败：{error}"), None));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程级隔离 store：与 render 测试同值（谁先谁设，同值无竞态）。
    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("KXEN_TRUST_FILE", std::env::temp_dir().join(format!("kxen-kn-trust-store-{}.json", std::process::id())));
        });
    }

    #[test]
    fn trust_roundtrip() {
        let dir = std::env::temp_dir().join(format!("kxen-trust-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trusted.json");
        assert!(load_from(&file).unwrap().is_empty());
        trust_into(&file, &dir).unwrap();
        assert!(load_from(&file).unwrap().iter().any(|p| p == &dir.to_string_lossy()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn visible_trust_reports_parent_sync_warning() {
        let dir = std::env::temp_dir().join(format!("kxen-trust-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trusted.json");
        FAIL_NEXT_TRUST_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let warning = trust_into(&file, &dir).unwrap();
        assert!(warning.as_deref().is_some_and(|warning| warning.contains("directory sync failure")));
        assert!(load_from(&file).unwrap().contains(&dir.to_string_lossy().into_owned()));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_store_blocks_trust_without_overwrite() {
        let dir = std::env::temp_dir().join(format!("kxen-trust-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("trusted.json");
        std::fs::write(&file, "{not json").unwrap();
        assert!(trust_into(&file, &dir).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{not json");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn worktree_inherits_project_trust() {
        setup();
        let dir = std::env::temp_dir().join(format!("kxen-trust-wt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        trust(&dir).unwrap();
        assert!(is_trusted(&dir.join(".kxen/worktrees/feat-x")), "worktree 副本继承项目信任");
        assert!(is_trusted(&dir.join(".kxen/worktrees/feat-x/src")), "worktree 内更深层同样继承");
        assert!(!is_trusted(&dir.join("src")), "普通子目录不继承");
        let other = std::env::temp_dir().join(format!("kxen-trust-wt-other-{}", std::process::id()));
        assert!(!is_trusted(&other), "无关目录不信任");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mcp_json_alone_triggers_gate() {
        // 仅含 .mcp.json 的项目（无 .agents、无 .kxen/config.toml）也必须触发信任门
        let dir = std::env::temp_dir().join(format!("kxen-trust-mcp-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!needs_gate(&dir));
        std::fs::write(dir.join(".mcp.json"), "{}").unwrap();
        assert!(needs_gate(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trust_prompt_discloses_code_execution_and_separate_stdio_approval() {
        assert!(TRUST_REASON.contains("执行本机代码"));
        assert!(TRUST_REASON.contains("独立审批"));
        assert!(TRUST_REASON.contains("command/args/cwd/env keys"));
    }
}

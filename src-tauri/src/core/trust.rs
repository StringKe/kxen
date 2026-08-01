//! 项目信任门：未信任 workspace 的项目知识（.agents rules/notes）只索引不进全文。
//! 决定持久化在 data_dir/trusted.json；审批走 ApprovalBroker（与 exec Ask 同一通道）。

use std::path::{Path, PathBuf};

pub type TrustCallback = std::sync::Arc<dyn Fn(&Path) + Send + Sync>;

fn store_file() -> PathBuf {
    // 测试隔离：环境变量覆盖（各测试模块用 Once 设同一值，写序防并行 env 竞态，勿删）
    if let Ok(p) = std::env::var("KXEN_TRUST_FILE") {
        return PathBuf::from(p);
    }
    crate::core::paths::data_dir().join("trusted.json")
}

fn load_from(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn trust_into(file: &Path, workdir: &Path) {
    // 读-改-写竞态防护：并发 trust 会互相覆盖丢失条目（并行测试抓出来的真 bug）
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = crate::core::shared::lock(&WRITE_LOCK);
    let mut list = load_from(file);
    let w = workdir.to_string_lossy().into_owned();
    if !list.contains(&w) {
        list.push(w);
        // 原子写（tmp+rename）：非原子写在并发 load_from 下会读到半截文件
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, serde_json::to_string_pretty(&list).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }
}

pub fn load() -> Vec<String> {
    load_from(&store_file())
}

pub fn is_trusted(workdir: &Path) -> bool {
    let w = workdir.to_string_lossy();
    load().iter().any(|p| {
        if p == &w {
            return true;
        }
        // 子路径继承仅限 <repo>/.kxen/worktrees/<name>：worktree 是同一项目副本，
        // 精确匹配会让 worktree 会话退回未信任（custom role/skill/command 全失效）；
        // 不收宽到任意子目录——子目录可能是拖进项目的不可信第三方代码
        workdir.starts_with(Path::new(p).join(".kxen/worktrees"))
    })
}

pub fn trust(workdir: &Path) {
    trust_into(&store_file(), workdir);
}

/// 项目内存在需要信任决策的内容（知识树或项目级 hooks 配置）。
pub fn needs_gate(workdir: &Path) -> bool {
    workdir.join(".agents").is_dir() || workdir.join(".kxen/config.toml").is_file()
}

/// 项目 hooks 死代码接线：已信任合并项目 .kxen/config.toml 重载，未信任回用户级。
pub fn reload_hooks_for_workspace(workdir: &Path, hooks: &crate::tools::hooks::HookRunner) {
    let project_cfg = if is_trusted(workdir) { Some(workdir.join(".kxen/config.toml")) } else { None };
    let merged = crate::core::config::Config::load(&crate::core::paths::config_dir().join("config.toml"), project_cfg.as_deref())
        .unwrap_or_default();
    hooks.reload(&merged);
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
        let (id, rx) = broker.register("", &dir.display().to_string(), "信任此项目？（.agents 知识与项目配置将注入模型上下文）");
        bus.publish(crate::core::event::Event::LlmDelta(serde_json::json!({
            "kind": "approval",
            "approval_id": id,
            "command": dir.display().to_string(),
            "reason": "信任此项目？（.agents 知识与项目配置将注入模型上下文）",
        })));
        let outcome = broker.wait(&id, rx, None).await;
        if matches!(outcome, crate::agent::approval::ApprovalOutcome::Allow) {
            trust(&dir);
            if let Some(callback) = on_trusted {
                callback(&dir);
            }
            bus.publish(crate::core::event::Event::notify(format!("已信任项目 {}", dir.display()), None));
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
        assert!(load_from(&file).is_empty());
        trust_into(&file, &dir);
        assert!(load_from(&file).iter().any(|p| p == &dir.to_string_lossy()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn worktree_inherits_project_trust() {
        setup();
        let dir = std::env::temp_dir().join(format!("kxen-trust-wt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        trust(&dir);
        assert!(is_trusted(&dir.join(".kxen/worktrees/feat-x")), "worktree 副本继承项目信任");
        assert!(is_trusted(&dir.join(".kxen/worktrees/feat-x/src")), "worktree 内更深层同样继承");
        assert!(!is_trusted(&dir.join("src")), "普通子目录不继承");
        let other = std::env::temp_dir().join(format!("kxen-trust-wt-other-{}", std::process::id()));
        assert!(!is_trusted(&other), "无关目录不信任");
        std::fs::remove_dir_all(&dir).ok();
    }
}

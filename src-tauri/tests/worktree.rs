// worktree 生命周期与审批门测试（从 tools/worktree.rs 拆出，350 行门禁）。
use kxen_app::agent::approval::ApprovalBroker;
use kxen_app::core::event::{Event, EventBus};
use kxen_app::tools::exec::ApprovalCtx;
use kxen_app::tools::worktree::{create, diff_stat, list, remove, remove_with_approval, validate_name};
use std::path::{Path, PathBuf};

/// 建临时 git 仓库（tag 区分并行测试，避免同 pid 撞目录；先清上次失败的残留）
fn init_repo(tag: &str) -> PathBuf {
    let repo = std::env::temp_dir().join(format!("kxen-wt-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git").args(args).current_dir(&repo).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&["init", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "hello").unwrap();
    run(&["add", "."]);
    run(&["-c", "user.email=t@t", "-c", "user.name=t", "-c", "commit.gpgsign=false", "commit", "-m", "init"]);
    repo
}

/// 同步跑 git 取 stdout（断言分支是否还存在）
fn git_out(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").args(args).current_dir(repo).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 挂总线等审批事件并按 allow 应答（join! 同任务轮询，避开 spawn 的 'static 约束）
async fn respond_via_bus<T>(broker: &ApprovalBroker, bus: &EventBus, allow: bool, fut: impl std::future::Future<Output = T>) -> T {
    let mut rx = bus.subscribe();
    let responder = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(Event::LlmDelta(v)) = rx.recv().await
                    && v.get("kind").and_then(|k| k.as_str()) == Some("approval")
                {
                    let id = v.get("approval_id").and_then(|i| i.as_str()).unwrap_or_default().to_string();
                    assert!(broker.respond(&id, allow), "approval should be pending");
                    return;
                }
            }
        })
        .await
        .expect("approval event not published");
    };
    let (out, ()) = tokio::join!(fut, responder);
    out
}

/// 用临时 git 仓库真实跑 create/list/remove。
#[tokio::test]
async fn lifecycle() {
    let repo = init_repo("lc");

    let info = create(&repo, "wt1").await.unwrap();
    assert!(info.path.join("a.txt").exists());
    assert_eq!(info.branch, "kxen/wt1");
    // .gitignore 幂等
    let gi = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(gi.matches(".kxen/").count(), 1);

    let trees = list(&repo).await.unwrap();
    assert_eq!(trees.len(), 1);
    assert_eq!(trees[0].name, "wt1");

    // 删分支必须过审批：用户放行后 worktree 与分支一起清掉
    let broker = ApprovalBroker::new();
    let bus = EventBus::default();
    let ctx = ApprovalCtx { broker: &broker, bus: &bus, cancel: None, session_id: "t" };
    respond_via_bus(&broker, &bus, true, remove_with_approval(&repo, "wt1", true, Some(&ctx), false)).await.unwrap();
    assert!(list(&repo).await.unwrap().is_empty());

    std::fs::remove_dir_all(&repo).ok();
}

#[test]
fn name_validation() {
    assert!(validate_name("wt-1_a").is_ok());
    for bad in ["", "..", "../x", "a/b", "a\\b", "a b", "a.b", "a;b"] {
        assert!(validate_name(bad).is_err(), "{bad}");
    }
}

/// 切进隔离树后 list 仍按主仓库根解析：active_workspace 是 worktree 路径时看板不得变空
#[tokio::test]
async fn list_resolves_main_root_from_inside_worktree() {
    let repo = init_repo("root");
    let info = create(&repo, "r1").await.unwrap();
    // 入参是 worktree 路径（切进树后的 active_workspace 形态）：结果与主仓库一致
    let trees = list(&info.path).await.unwrap();
    assert_eq!(trees.len(), 1, "从 worktree 内 list 不得变空");
    assert_eq!(trees[0].name, "r1");
    assert_eq!(trees[0].path, info.path);
    // 主仓库视角结果相同
    assert_eq!(list(&repo).await.unwrap().len(), 1);
    std::fs::remove_dir_all(&repo).ok();
}

#[tokio::test]
async fn remove_and_diff_reject_bad_names() {
    let repo = init_repo("bad");
    for bad in ["../x", "a/b", ".."] {
        assert!(remove(&repo, bad, false).await.unwrap_err().contains("invalid worktree name"), "{bad}");
        assert!(diff_stat(&repo, bad).await.unwrap_err().contains("invalid worktree name"), "{bad}");
    }
    std::fs::remove_dir_all(&repo).ok();
}

/// clean 且保留分支：无数据可丢，无审批通道的旧入口直接放行（分支仍在）
#[tokio::test]
async fn clean_remove_without_channel_ok() {
    let repo = init_repo("clean");
    create(&repo, "c1").await.unwrap();
    remove(&repo, "c1", false).await.unwrap();
    assert!(!repo.join(".kxen/worktrees/c1").exists());
    assert!(git_out(&repo, &["branch", "--list", "kxen/c1"]).contains("kxen/c1"));
    std::fs::remove_dir_all(&repo).ok();
}

/// delete_branch 无通道按拒绝，审批放行后才真删
#[tokio::test]
async fn delete_branch_requires_approval() {
    let repo = init_repo("delbr");
    create(&repo, "b1").await.unwrap();
    let err = remove(&repo, "b1", true).await.unwrap_err();
    assert!(err.contains("审批通道"), "{err}");
    assert!(repo.join(".kxen/worktrees/b1").exists());
    assert!(git_out(&repo, &["branch", "--list", "kxen/b1"]).contains("kxen/b1"));

    let broker = ApprovalBroker::new();
    let bus = EventBus::default();
    let ctx = ApprovalCtx { broker: &broker, bus: &bus, cancel: None, session_id: "t" };
    respond_via_bus(&broker, &bus, true, remove_with_approval(&repo, "b1", true, Some(&ctx), false)).await.unwrap();
    assert!(!repo.join(".kxen/worktrees/b1").exists());
    assert!(git_out(&repo, &["branch", "--list", "kxen/b1"]).trim().is_empty());
    std::fs::remove_dir_all(&repo).ok();
}

/// dirty（未跟踪文件也算）：无通道拒绝、用户拒绝都保留现场，用户放行才删
#[tokio::test]
async fn dirty_remove_guarded_by_approval() {
    let repo = init_repo("dirty");
    create(&repo, "d1").await.unwrap();
    let wt = repo.join(".kxen/worktrees/d1");
    std::fs::write(wt.join("dirty.txt"), "x").unwrap();

    let err = remove(&repo, "d1", false).await.unwrap_err();
    assert!(err.contains("审批通道"), "{err}");
    assert!(wt.join("dirty.txt").exists());

    let broker = ApprovalBroker::new();
    let bus = EventBus::default();
    let ctx = ApprovalCtx { broker: &broker, bus: &bus, cancel: None, session_id: "t" };
    let err = respond_via_bus(&broker, &bus, false, remove_with_approval(&repo, "d1", false, Some(&ctx), false)).await.unwrap_err();
    assert!(err.contains("用户拒绝"), "{err}");
    assert!(wt.join("dirty.txt").exists());

    respond_via_bus(&broker, &bus, true, remove_with_approval(&repo, "d1", false, Some(&ctx), false)).await.unwrap();
    assert!(!wt.exists());
    std::fs::remove_dir_all(&repo).ok();
}

/// confirmed（前端行内确认条已显式确认）：dirty/删分支都不再挂审批，无通道也直接执行；
/// agent 工具路径恒 confirmed=false，审批语义不受影响（见 dirty_remove_guarded_by_approval）
#[tokio::test]
async fn confirmed_skips_approval() {
    let repo = init_repo("conf");
    create(&repo, "cf1").await.unwrap();
    let wt = repo.join(".kxen/worktrees/cf1");
    std::fs::write(wt.join("dirty.txt"), "x").unwrap();

    // 无审批通道 + confirmed：不拒绝、不挂起，直接删（连分支一起）
    remove_with_approval(&repo, "cf1", true, None, true).await.unwrap();
    assert!(!wt.exists());
    assert!(git_out(&repo, &["branch", "--list", "kxen/cf1"]).trim().is_empty());
    std::fs::remove_dir_all(&repo).ok();
}

/// 残留同名分支（remove 保留分支后重建同名 worktree）：复用该分支而不是报错
#[tokio::test]
async fn create_reuses_leftover_branch() {
    let repo = init_repo("rebr");
    create(&repo, "rb1").await.unwrap();
    remove(&repo, "rb1", false).await.unwrap(); // 分支保留
    assert!(git_out(&repo, &["branch", "--list", "kxen/rb1"]).contains("kxen/rb1"));

    let info = create(&repo, "rb1").await.unwrap();
    assert_eq!(info.branch, "kxen/rb1");
    assert!(info.path.join("a.txt").exists(), "复用分支重建的 worktree 必须有内容");
    std::fs::remove_dir_all(&repo).ok();
}

/// .kxen/backups 数量上限：超出保留最近 50 份，最旧的被清掉
#[test]
fn prune_backups_keeps_newest() {
    use kxen_app::tools::worktree::prune_backups;
    let dir = std::env::temp_dir().join(format!("kxen-wt-prune-{}", std::process::id()));
    let backups = dir.join(".kxen/backups");
    std::fs::create_dir_all(&backups).unwrap();
    // f000 最先写且 sleep 隔开：mtime 严格最旧，淘汰必命中它
    std::fs::write(backups.join("f000.kxen-bak"), "x").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    for i in 1..60 {
        std::fs::write(backups.join(format!("f{i:03}.kxen-bak")), "x").unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    std::fs::write(backups.join("newest.kxen-bak"), "x").unwrap();
    prune_backups(&dir);
    let left = std::fs::read_dir(&backups).unwrap().count();
    assert_eq!(left, 50, "必须只保留最近 50 份");
    assert!(!backups.join("f000.kxen-bak").exists(), "最旧的被清掉");
    assert!(backups.join("newest.kxen-bak").exists(), "最新的保留");
    std::fs::remove_dir_all(&dir).ok();
}

/// RPC 边界原语：workspace 内放行（相对/绝对），越界路径被拒
#[test]
fn resolve_in_workspace_rejects_escape() {
    use kxen_app::tools::worktree::resolve_in_workspace;
    let dir = std::env::temp_dir().join(format!("kxen-wt-bound-{}", std::process::id()));
    let work = dir.join("work");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "s").unwrap();
    let work = work.canonicalize().unwrap();
    let grants = std::collections::HashSet::new();

    assert!(resolve_in_workspace("a.txt", &work, &grants).is_ok(), "workspace 内相对路径放行");
    let abs = outside.join("secret.txt").canonicalize().unwrap();
    let err = resolve_in_workspace(abs.to_str().unwrap(), &work, &grants).unwrap_err();
    assert!(err.contains("escapes workspace"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

//! exec 真实验证：快命令前台、长命令 auto_bg、rm -> trash 遮蔽、safety 拦截。

use kxen_app::tools::exec::{ExecOutcome, ExecParams, exec};
use kxen_app::tools::shell::ShellKind;
use kxen_app::tools::task::{TaskOwner, TaskRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let registry = Arc::new(TaskRegistry::new());
    let cwd = std::env::temp_dir().join(format!("kxen-exec-demo-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let owner = TaskOwner::new("tools-exec-demo", &cwd).unwrap();
    println!("cwd: {}", cwd.display());

    // 1. 快命令前台
    let out = exec(
        ExecParams {
            shell_type: ShellKind::Zsh,
            path: cwd.display().to_string(),
            command: "echo hello-kxen && pwd".into(),
            timeout_ms: None,
            background: false,
        },
        &registry,
        &cwd.display().to_string(),
        &owner,
        None,
    )
    .await
    .unwrap();
    println!("[foreground] {out:?}");

    // 2. safety 拦截
    let blocked = exec(
        ExecParams { shell_type: ShellKind::Zsh, path: "/".into(), command: "rm -rf /".into(), timeout_ms: None, background: false },
        &registry,
        &cwd.display().to_string(),
        &owner,
        None,
    )
    .await;
    println!("[blocked] {blocked:?}");

    // 3. rm -> trash 遮蔽（创建文件后 rm，应进回收站而非真删）
    let probe = cwd.join("probe.txt");
    std::fs::write(&probe, "to-be-trashed").unwrap();
    let out = exec(
        ExecParams {
            shell_type: ShellKind::Zsh,
            path: cwd.display().to_string(),
            command: "rm probe.txt; ls probe.txt 2>&1 || echo ABSENT; ls ~/.Trash/probe.txt 2>/dev/null && echo IN_TRASH".to_string(),
            timeout_ms: None,
            background: false,
        },
        &registry,
        &cwd.display().to_string(),
        &owner,
        None,
    )
    .await
    .unwrap();
    println!("[trash-test] {out:?}");

    // 4. 长命令 auto_bg（sleep 30，15s 预算内应转后台）
    let out = exec(
        ExecParams {
            shell_type: ShellKind::Zsh,
            path: cwd.display().to_string(),
            command: "sleep 30 && echo late".into(),
            timeout_ms: None,
            background: false,
        },
        &registry,
        &cwd.display().to_string(),
        &owner,
        None,
    )
    .await
    .unwrap();
    match &out {
        ExecOutcome::Background { task_id } => {
            println!("[auto-bg] task_id = {task_id}");
            let info = registry.get(&owner, task_id).unwrap();
            println!("[auto-bg] status = {:?}, command = {}", info.status(), info.command);
            registry.kill(&owner, task_id).await;
        }
        other => println!("[auto-bg] UNEXPECTED: {other:?}"),
    }

    std::fs::remove_dir_all(&cwd).ok();
}

//! dev_server 真实验证：起一个 http.server，就绪检测 + list + restart + kill。

use kxen_app::tools::dev_server::{DevServerParams, ReadySpec, dev_server, restart_task};
use kxen_app::tools::task::{TaskOwner, TaskRegistry};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let registry = Arc::new(TaskRegistry::new());
    let cwd = std::env::temp_dir().join(format!("kxen-devdemo-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let owner = TaskOwner::new("tools-dev-server-demo", &cwd).unwrap();

    // 起一个简单的 python http.server（输出含 "Serving HTTP"）
    let started = dev_server(
        DevServerParams {
            command: "python3 -m http.server 18923".into(),
            workdir: cwd.display().to_string(),
            ready: Some(ReadySpec { pattern: Some("Serving HTTP".into()), port: Some(18923), timeout_ms: Some(10_000) }),
            shell: None,
        },
        &registry,
        &owner,
    )
    .await
    .unwrap();
    println!("[ready] task_id={} url={:?} pid={:?}", started.task_id, started.url, started.pid);

    // list 可见
    let list = registry.list(&owner);
    println!("[list] {} task(s):", list.len());
    for t in &list {
        println!("  {} {:?} uptime={}ms port={:?}", t.id, t.status, t.uptime_ms, t.port);
    }

    // restart
    let new_id = restart_task(&started.task_id, &owner, &registry).await.unwrap();
    println!("[restart] {} -> {}", started.task_id, new_id);
    let list2 = registry.list(&owner);
    println!("[list-after-restart] {} task(s)", list2.len());

    // kill
    registry.kill(&owner, &new_id).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let out = registry.output(&owner, &new_id);
    println!("[killed] {:?}", out.map(|(_, _, s)| s));

    std::fs::remove_dir_all(&cwd).ok();
}

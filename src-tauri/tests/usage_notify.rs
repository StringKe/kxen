//! usage 跨 request 累加 + 通知循环 Lagged 存活回归。

use kxen_app::agent::agent_loop::UsageAcc;
use kxen_app::core::event::{Event, EventBus, RecvVerdict, recv_verdict};

#[test]
fn usage_accumulates_across_requests() {
    let mut acc = UsageAcc::default();
    assert_eq!(acc.total(), (0, 0));
    assert_eq!(acc.last_input(), 0);
    // 多轮 tool loop：每轮各一次 Usage，覆盖式只记末轮是漏算根因
    acc.push(100, 20);
    acc.push(180, 40);
    acc.push(260, 30);
    assert_eq!(acc.total(), (540, 90), "input/output 必须跨 request 累加");
    assert_eq!(acc.last_input(), 260, "ctx 占用取最近一次 request 而非累计值");
}

#[test]
fn goal_delta_charges_each_turn_exactly_once() {
    let mut acc = UsageAcc::default();
    acc.push(100, 20);
    assert_eq!(acc.goal_delta(), 120, "首轮按全额入账");
    acc.push(180, 40);
    assert_eq!(acc.goal_delta(), 220, "次轮只按增量入账，累计值不得重复计");
    assert_eq!(acc.goal_delta(), 0, "无新 usage 不重复入账");
}

#[tokio::test]
async fn lagged_consumer_skips_and_keeps_receiving() {
    // 小 capacity 构造 lag：通知落盘循环遇 Lagged 必须跳过继续（静默退出 = 通知中心永久停更）
    let bus = EventBus::new(4);
    let mut rx = bus.subscribe();
    for i in 0..6 {
        bus.publish(Event::notify(format!("n{i}"), None));
    }
    let first = rx.recv().await;
    assert!(matches!(recv_verdict(first), RecvVerdict::Skip), "溢出必须先判 Skip");
    // lag 之后仍能收到后续事件：循环存活
    bus.publish(Event::notify("after", None));
    let mut survived = false;
    for _ in 0..8 {
        if let RecvVerdict::Event(Event::Notification { text, .. }) = recv_verdict(rx.recv().await)
            && text == "after"
        {
            survived = true;
            break;
        }
    }
    assert!(survived, "lag 后必须能继续收到新事件");
    // bus 关闭（app 退出）才 Stop
    drop(bus);
    loop {
        match recv_verdict(rx.recv().await) {
            RecvVerdict::Event(_) => continue,
            RecvVerdict::Skip => continue,
            RecvVerdict::Stop => break,
        }
    }
}

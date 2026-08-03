use super::*;
use crate::core::config::{Config, ProviderLimit};
use crate::llm::Delta;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn mrm_with_circuit_threshold(threshold: u32) -> crate::llm::mrm::ModelResourceManager {
    let mut config = Config::default();
    config.limits.providers.insert(
        "xai".into(),
        ProviderLimit { circuit_failure_threshold: Some(threshold), circuit_cooldown_seconds: Some(600), ..Default::default() },
    );
    crate::llm::mrm::ModelResourceManager::new(config)
}

#[tokio::test]
async fn local_dispatch_failure_is_not_counted_as_a_provider_request() {
    let mrm = mrm_with_circuit_threshold(1);
    let error = collect_text_observed(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_secs(1),
        None,
        None,
    )
    .await
    .expect_err("missing local credential must fail before Provider dispatch");

    assert_eq!(error.kind, ManagedErrorKind::Local);
    assert!(!error.request_started);
    assert!(!error.usage_reported);
    assert!(mrm.admit("xai").await.is_ok(), "local validation must not consume or poison MRM state");
}

#[tokio::test]
async fn rejects_open_circuit_before_starting_stream() {
    let mrm = mrm_with_circuit_threshold(1);
    mrm.record_result("xai", false).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Delta::Text("unexpected".into()), Delta::Done]))
    });

    let error = collect_text(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_secs(1),
        Some(&stream),
        None,
    )
    .await
    .expect_err("open circuit must reject utility request");

    assert!(error.contains("circuit open"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn collects_text_through_managed_stream() {
    let mrm = mrm_with_circuit_threshold(3);
    let stream: StreamFn =
        Arc::new(|_, _, _, _| Box::pin(futures::stream::iter(vec![Delta::Text("po".into()), Delta::Text("ng".into()), Delta::Done])));

    let output = collect_text(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_secs(1),
        Some(&stream),
        None,
    )
    .await
    .expect("managed request");

    assert_eq!(output, ManagedOutput { text: "pong".into(), usage: None, metering_warning: None });
}

#[tokio::test]
async fn local_queue_timeout_does_not_open_provider_circuit() {
    let mut config = Config::default();
    config.limits.global_concurrent = 1;
    config.limits.providers.insert(
        "xai".into(),
        ProviderLimit {
            concurrent: Some(1),
            rpm: Some(1),
            circuit_failure_threshold: Some(1),
            circuit_cooldown_seconds: Some(600),
            ..Default::default()
        },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    let _held = mrm.acquire_slot("xai").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Delta::Done]))
    });

    let error = collect_text(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_millis(10),
        Some(&stream),
        None,
    )
    .await
    .expect_err("full local queue must time out");

    assert!(error.contains("queue timed out"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(mrm.admit("xai").await.is_ok(), "local queue pressure is not a provider failure");
    assert!(!mrm.rpm_blocked("xai").await, "a request that never started must not consume RPM");
}

#[tokio::test]
async fn cancellation_does_not_open_provider_circuit() {
    let mrm = mrm_with_circuit_threshold(1);
    let cancel = crate::agent::cancel::CancelToken::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::pending())
    });
    let model = ModelRef::new("xai", "grok");
    let messages = [Message::user("ping")];
    let store = crate::auth::credential::AuthStore::default();
    let request = collect_text(&mrm, &model, &messages, &store, Duration::from_secs(1), Some(&stream), Some(&cancel));
    let cancel_request = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    };

    let (result, ()) = tokio::join!(request, cancel_request);

    assert!(result.expect_err("cancelled request must stop").contains("cancelled"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(mrm.admit("xai").await.is_ok(), "user cancellation is not a provider failure");
}

#[tokio::test]
async fn admission_is_rechecked_after_waiting_for_a_slot() {
    let mut config = Config::default();
    config.limits.global_concurrent = 1;
    config.limits.providers.insert(
        "xai".into(),
        ProviderLimit {
            concurrent: Some(1),
            rpm: Some(1),
            circuit_failure_threshold: Some(1),
            circuit_cooldown_seconds: Some(600),
            ..Default::default()
        },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    let held = mrm.acquire_slot("xai").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Delta::Done]))
    });
    let model = ModelRef::new("xai", "grok");
    let messages = [Message::user("ping")];
    let store = crate::auth::credential::AuthStore::default();
    let request = collect_text(&mrm, &model, &messages, &store, Duration::from_secs(1), Some(&stream), None);
    let open_circuit = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        mrm.record_result("xai", false).await;
        drop(held);
    };

    let (result, ()) = tokio::join!(request, open_circuit);

    assert!(result.expect_err("post-queue admission must reject").contains("circuit open"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!mrm.rpm_blocked("xai").await, "post-queue rejection must roll back RPM reservation");
}

#[tokio::test]
async fn cancellation_while_queued_does_not_consume_rpm() {
    let mut config = Config::default();
    config.limits.global_concurrent = 1;
    config.limits.providers.insert("xai".into(), ProviderLimit { concurrent: Some(1), rpm: Some(1), ..Default::default() });
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    let held = mrm.acquire_slot("xai").await;
    let cancel = crate::agent::cancel::CancelToken::new();
    let stream: StreamFn = Arc::new(|_, _, _, _| Box::pin(futures::stream::iter(vec![Delta::Done])));
    let model = ModelRef::new("xai", "grok");
    let messages = [Message::user("ping")];
    let store = crate::auth::credential::AuthStore::default();
    let request = collect_text(&mrm, &model, &messages, &store, Duration::from_secs(1), Some(&stream), Some(&cancel));
    let cancel_request = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    };

    let (result, ()) = tokio::join!(request, cancel_request);
    drop(held);

    assert!(is_cancelled_error(&result.expect_err("queued request must cancel")));
    assert!(!mrm.rpm_blocked("xai").await, "queued cancellation must not consume RPM");
}

#[tokio::test]
async fn queue_and_stream_share_one_total_deadline() {
    let mut config = Config::default();
    config.limits.global_concurrent = 1;
    config
        .limits
        .providers
        .insert("xai".into(), ProviderLimit { concurrent: Some(1), circuit_failure_threshold: Some(1), ..Default::default() });
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    let held = mrm.acquire_slot("xai").await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::pending())
    });
    let model = ModelRef::new("xai", "grok");
    let messages = [Message::user("ping")];
    let store = crate::auth::credential::AuthStore::default();
    let started = std::time::Instant::now();
    let request = collect_text(&mrm, &model, &messages, &store, Duration::from_millis(200), Some(&stream), None);
    let release = async {
        tokio::time::sleep(Duration::from_millis(180)).await;
        drop(held);
    };

    let (result, ()) = tokio::join!(request, release);

    assert!(result.expect_err("local queue should preserve a Provider time floor").contains("local resource queue exhausted"));
    assert!(started.elapsed() < Duration::from_millis(300), "queue time must consume the same timeout budget");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a Provider request must not start with only deadline scraps left");
    assert!(mrm.admit("xai").await.is_ok(), "local queue exhaustion must not open the Provider circuit");
}

#[tokio::test]
async fn neutral_probe_failure_does_not_open_shared_circuit() {
    let mrm = mrm_with_circuit_threshold(1);
    let stream: StreamFn = Arc::new(|_, _, _, _| Box::pin(futures::stream::iter(vec![Delta::Error("invalid probe key".into())])));

    let error = collect_text_with_policy(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_secs(1),
        Some(&stream),
        None,
        CircuitPolicy::Neutral,
    )
    .await
    .expect_err("probe should surface credential failure");

    assert_eq!(error, "invalid probe key");
    assert!(mrm.admit("xai").await.is_ok(), "temporary probe credentials must not poison the shared circuit");
}

#[tokio::test]
async fn neutral_probe_can_test_recovery_while_shared_circuit_is_open() {
    let mrm = mrm_with_circuit_threshold(1);
    mrm.record_result("xai", false).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter(vec![Delta::Text("ok".into()), Delta::Done]))
    });

    let output = collect_text_with_policy(
        &mrm,
        &ModelRef::new("xai", "grok"),
        &[Message::user("ping")],
        &Default::default(),
        Duration::from_secs(1),
        Some(&stream),
        None,
        CircuitPolicy::Neutral,
    )
    .await
    .expect("probe should bypass only circuit state");

    assert_eq!(output.text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(mrm.admit("xai").await.is_err(), "neutral probe success must not reset the saved Provider circuit");
}

#[tokio::test]
async fn half_open_allows_only_one_managed_provider_stream() {
    let mut config = Config::default();
    config.limits.providers.insert(
        "xai".into(),
        ProviderLimit { concurrent: Some(2), circuit_failure_threshold: Some(1), circuit_cooldown_seconds: Some(0), ..Default::default() },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    mrm.record_result("xai", false).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let seen = calls.clone();
    let signal = release.clone();
    let stream: StreamFn = Arc::new(move |_, _, _, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        let signal = signal.clone();
        Box::pin(futures::stream::once(async move {
            signal.notified().await;
            Delta::Done
        }))
    });
    let model = ModelRef::new("xai", "grok");
    let messages = [Message::user("ping")];
    let store = crate::auth::credential::AuthStore::default();
    let first = collect_text(&mrm, &model, &messages, &store, Duration::from_secs(1), Some(&stream), None);
    let second = async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let result = collect_text(&mrm, &model, &messages, &store, Duration::from_secs(1), Some(&stream), None).await;
        release.notify_waiters();
        result
    };

    let (first, second) = tokio::join!(first, second);

    first.expect("first half-open probe");
    assert!(second.expect_err("second probe must be rejected").contains("probe in progress"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

use super::*;

#[test]
fn status_distinguishes_configured_credentials() {
    let mut store = AuthStore::new();
    let initial = statuses(&crate::core::config::Config::default(), &store);
    assert!(initial.iter().all(|status| status.status == "unconfigured" && status.detail.contains("未配置")));
    store.insert(store_key("xai"), CredentialKind::Api { key: "k".into(), region: None });
    assert_eq!(statuses(&crate::core::config::Config::default(), &store).iter().find(|status| status.id == "xai").unwrap().status, "ready");
}

#[test]
fn postcommit_warning_publishes_visible_voice_key_to_memory() {
    let root = std::env::temp_dir().join(format!("kxen-voice-key-postcommit-{}", uuid::Uuid::new_v4()));
    let path = root.join("auth.json");
    let mut store = AuthStore::new();
    crate::auth::credential::write_auth_file(&path, &store).unwrap();
    crate::auth::credential::fail_next_auth_dir_sync();
    let error = set_key(&mut store, "xai", "new-key", &path).expect_err("postcommit durability uncertainty must surface");
    assert!(error.contains("durability is indeterminate"), "{error}");
    assert_eq!(crate::auth::credential::read_auth_file(&path).unwrap()["voice:xai"].bearer(), "new-key");
    assert_eq!(store["voice:xai"].bearer(), "new-key");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn media_request_settles_as_unknown_not_zero_tokens() {
    let root = std::env::temp_dir().join(format!("kxen-voice-meter-{}", uuid::Uuid::new_v4()));
    let usage = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
        "system_voice",
        usage.clone(),
        crate::core::event::EventBus::default(),
        root.clone(),
    );
    let mut attempt = reporter.begin(None).unwrap();
    reporter.mark_started(&mut attempt).unwrap();
    assert_eq!(settle_transcription(Ok("hello".into()), &reporter, &attempt).unwrap(), "hello");
    let usage = crate::core::shared::lock(&usage)["system_voice"].clone();
    assert_eq!((usage.input, usage.output, usage.unmetered_calls), (0, 0, 1));
    assert!(crate::core::usage::ProviderAttemptStore::new(root.clone()).load_all().unwrap().is_empty());
    std::fs::remove_file(root.with_extension("usage.json")).ok();
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn mrm_admission_failure_creates_no_attempt_and_sends_no_request() {
    let root = std::env::temp_dir().join(format!("kxen-voice-admission-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let wav = root.join("voice.wav");
    write_wav(&wav, &[0.0], 16_000).unwrap();
    let mut config = crate::core::config::Config::default();
    config.custom_providers.insert(
        "blocked_audio".into(),
        crate::core::config::CustomProviderDef {
            base_url: "http://127.0.0.1:9".into(),
            protocol: "openai".into(),
            models: vec!["audio".into()],
            capabilities: vec!["audio".into()],
        },
    );
    config.limits.providers.insert(
        "custom:blocked_audio".into(),
        crate::core::config::ProviderLimit { circuit_failure_threshold: Some(1), ..Default::default() },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config.clone());
    mrm.record_result("custom:blocked_audio", false).await;
    let mut auth = AuthStore::new();
    auth.insert("custom:blocked_audio".into(), CredentialKind::Api { key: "k".into(), region: None });
    let attempts = root.join("attempts");
    let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
        "system_voice",
        std::sync::Arc::default(),
        crate::core::event::EventBus::default(),
        attempts.clone(),
    );
    let error = transcribe_file(&config, &auth, "custom:blocked_audio", wav.to_str().unwrap(), &mrm, &reporter)
        .await
        .expect_err("open circuit must reject before request start");
    assert!(error.contains("MRM admission"), "{error}");
    assert!(crate::core::usage::ProviderAttemptStore::new(attempts).load_all().unwrap().is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn started_transcription_records_mrm_failure_and_unknown_usage() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request);
        socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\nconnection: close\r\n\r\nfail").unwrap();
    });
    let root = std::env::temp_dir().join(format!("kxen-voice-started-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let wav = root.join("voice.wav");
    write_wav(&wav, &[0.0], 16_000).unwrap();
    let mut config = crate::core::config::Config::default();
    config.custom_providers.insert(
        "live_audio".into(),
        crate::core::config::CustomProviderDef {
            base_url: format!("http://{address}"),
            protocol: "openai".into(),
            models: vec!["audio".into()],
            capabilities: vec!["audio".into()],
        },
    );
    config.limits.providers.insert(
        "custom:live_audio".into(),
        crate::core::config::ProviderLimit { circuit_failure_threshold: Some(1), ..Default::default() },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config.clone());
    let mut auth = AuthStore::new();
    auth.insert("custom:live_audio".into(), CredentialKind::Api { key: "k".into(), region: None });
    let usage = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let attempts = root.join("attempts");
    let reporter = crate::agent::agent_loop::UsageReporter::new_unscoped_in(
        "system_voice",
        usage.clone(),
        crate::core::event::EventBus::default(),
        attempts.clone(),
    );

    let error = transcribe_file(&config, &auth, "custom:live_audio", wav.to_str().unwrap(), &mrm, &reporter)
        .await
        .expect_err("remote 500 must fail");
    assert!(error.contains("HTTP 500"), "{error}");
    assert!(mrm.begin_call("custom:live_audio", None).await.err().expect("circuit must open").contains("circuit"));
    assert_eq!(crate::core::shared::lock(&usage)["system_voice"].unmetered_calls, 1);
    assert!(crate::core::usage::ProviderAttemptStore::new(attempts.clone()).load_all().unwrap().is_empty());
    std::fs::remove_file(attempts.with_extension("usage.json")).ok();
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn wav_and_pcm_limits_are_enforced() {
    let dir = std::env::temp_dir().join(format!("kxen-wav-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.wav");
    write_wav(&path, &[0.0, 0.5, -0.5], 16_000).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(bytes.len(), 50);

    let mut buffer = SampleBuffer::default();
    append_samples_up_to(&mut buffer, &[1.0, 2.0], 3);
    append_samples_up_to(&mut buffer, &[3.0, 4.0], 3);
    assert_eq!(buffer.samples, vec![1.0, 2.0, 3.0]);
    assert!(buffer.exceeded);
    assert_eq!(sample_limit(16_000), 16_000 * MAX_AUDIO_SECONDS);
    assert_eq!(sample_limit(96_000), 96_000 * MAX_AUDIO_SECONDS);
    assert_eq!(sample_limit(192_000), MAX_PCM_SAMPLES);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn wav_writer_and_reader_reject_over_limit_inputs() {
    let path = temp_wav_path();
    let samples = vec![0.0; MAX_AUDIO_SECONDS + 1];
    assert_eq!(write_wav(&path, &samples, 1).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    assert!(!path.exists());
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_WAV_BYTES as u64 + 1).unwrap();
    assert!(read_bounded_wav(&path).unwrap_err().contains("超过"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn temp_wav_path_is_unique() {
    let first = temp_wav_path();
    let second = temp_wav_path();
    assert_ne!(first, second);
    assert!(first.file_name().unwrap().to_string_lossy().starts_with("kxen-voice-"));
}

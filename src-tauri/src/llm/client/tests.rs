use super::*;
use crate::auth::credential::CredentialKind;
use crate::core::config::CustomProviderDef;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn custom_provider_named_account_uses_the_named_credential() {
    let mut store = crate::auth::credential::AuthStore::default();
    store.insert("custom:lab".into(), CredentialKind::Api { key: "default-key".into(), region: None });
    store.insert("custom:lab:work".into(), CredentialKind::Api { key: "work-key".into(), region: None });

    let credential = crate::auth::credential::credential_for(&store, "custom:lab", Some("work"));

    assert!(matches!(credential, Some(CredentialKind::Api { key, .. }) if key == "work-key"));
}

#[test]
fn custom_dispatch_rejects_invalid_endpoint_and_header_before_request() {
    let invalid_endpoint = CustomProviderDef {
        base_url: "https://".into(),
        models: vec!["model".into()],
        protocol: "openai".into(),
        capabilities: vec!["text".into()],
    };
    let error = validate_custom_dispatch(&invalid_endpoint, "valid-key").expect_err("missing host must fail locally");
    assert!(error.contains("base_url"));

    let valid_endpoint = CustomProviderDef { base_url: "https://api.example.com/v1".into(), ..invalid_endpoint };
    let error =
        validate_custom_dispatch(&valid_endpoint, "secret\r\ninjected: true").expect_err("invalid authorization header must fail locally");
    assert!(error.contains("header"));
}

#[test]
fn custom_dispatch_uses_workspace_mrm_definition() {
    let mut config = crate::core::config::Config::default();
    config.custom_providers.insert(
        "workspace".into(),
        CustomProviderDef {
            base_url: "https://workspace.example/v1".into(),
            models: vec!["model".into()],
            protocol: "openai".into(),
            capabilities: vec!["text".into()],
        },
    );
    let mrm = crate::llm::mrm::ModelResourceManager::new(config);
    let mut store = crate::auth::credential::AuthStore::default();
    store.insert("custom:workspace".into(), CredentialKind::Api { key: "workspace-key".into(), region: None });
    let model = crate::llm::ModelRef::new("custom:workspace", "model");

    LlmClient::validate_dispatch_in(&model, &store, None, Some(&mrm)).expect("workspace custom provider must resolve");
}

#[tokio::test]
async fn shared_http_does_not_follow_redirects() {
    let second_hop = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let second_hop_url = format!("http://{}/credential-sink", second_hop.local_addr().unwrap());
    let second_hop_task = tokio::spawn(async move {
        let accepted = tokio::time::timeout(std::time::Duration::from_millis(300), second_hop.accept()).await;
        let Ok(Ok((mut socket, _))) = accepted else { return false };
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        let _ = socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
        true
    });

    let redirect = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let redirect_url = format!("http://{}/start", redirect.local_addr().unwrap());
    let redirect_task = tokio::spawn(async move {
        let (mut socket, _) = redirect.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        let response =
            format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: {second_hop_url}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let response = shared_http().get(redirect_url).bearer_auth("must-not-leak").send().await.unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    redirect_task.await.unwrap();
    assert!(!second_hop_task.await.unwrap(), "shared HTTP client must not reach a redirect target");
}

#[tokio::main]
async fn main() {
    let store = kxen_app::auth::credential::read_auth_file(&kxen_app::core::paths::auth_file()).expect("read auth store");
    let Some(kxen_app::auth::credential::CredentialKind::Api { key, .. }) = store.get("kimi-for-coding") else {
        panic!("no kimi key");
    };
    let http = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "kimi-for-coding",
        "messages": [{"role": "user", "content": "say pong"}],
        "stream": true
    });
    println!("key len: {} | prefix: {}", key.len(), &key[..20]);
    let resp = http
        .post("https://api.kimi.com/coding/v1/chat/completions")
        .bearer_auth(key)
        .header("user-agent", "curl/8.7.1")
        .json(&body)
        .send()
        .await
        .unwrap();
    println!("status: {}", resp.status());
    println!("body: {}", resp.text().await.unwrap_or_default());
}

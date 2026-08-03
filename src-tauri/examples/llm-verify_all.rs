//! 九家真实调用验证（每家一次真实 API 调用）。

use kxen_app::llm::{Message, ModelRef};

#[tokio::main]
async fn main() {
    let auth_path = kxen_app::core::paths::auth_file();
    let mut store = kxen_app::auth::credential::read_auth_file(&auth_path).expect("read auth store");
    kxen_app::auth::probe_all(&mut store, true);
    let config_path = kxen_app::core::paths::config_dir().join("config.toml");
    let config = match kxen_app::core::config::Config::load(&config_path, None) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("FAIL  config -> {error}");
            return;
        }
    };
    let mrm = kxen_app::llm::mrm::ModelResourceManager::new(config);

    // model 与 providers registry 的 default_model 对齐
    let cases = [
        ("anthropic", "claude-sonnet-4-5-20250929"),
        ("openai", "gpt-5.4"),
        ("xai", "grok-build-0.1"),
        ("kimi-for-coding", "kimi-for-coding"),
        ("deepseek", "deepseek-chat"),
        ("mistral", "mistral-large-latest"),
        ("groq", "llama-3.3-70b-versatile"),
        ("google", "gemini-2.5-flash"),
        ("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
    ];

    for (provider, model) in cases {
        let model_ref = ModelRef::new(provider, model);
        let messages = vec![Message::user("Reply with exactly one word: pong")];
        match kxen_app::llm::managed::collect_text(&mrm, &model_ref, &messages, &store, std::time::Duration::from_secs(60), None, None)
            .await
        {
            Ok(output) => println!("PASS  {provider:18} {model:28} -> {}", output.text.trim()),
            Err(error) => println!("FAIL  {provider:18} {model:28} -> {error}"),
        }
    }
}

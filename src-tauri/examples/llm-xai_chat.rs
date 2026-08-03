//! xai 真实流式调用验证（用 probe 导入的凭证）。

use kxen_app::llm::{Message, ModelRef};

#[tokio::main]
async fn main() {
    let auth_path = kxen_app::core::paths::auth_file();
    let mut store = kxen_app::auth::credential::read_auth_file(&auth_path).expect("read auth store");
    let outcomes = kxen_app::auth::probe_all(&mut store, true);
    for (p, o, _) in &outcomes {
        eprintln!("probe {p}: {o:?}");
    }

    let config_path = kxen_app::core::paths::config_dir().join("config.toml");
    let config = match kxen_app::core::config::Config::load(&config_path, None) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("FAIL  config -> {error}");
            return;
        }
    };
    let mrm = kxen_app::llm::mrm::ModelResourceManager::new(config);
    let model = ModelRef::new("xai", "grok-build-0.1");
    let messages = vec![Message::user("Reply with exactly one word: pong")];
    match kxen_app::llm::managed::collect_text(&mrm, &model, &messages, &store, std::time::Duration::from_secs(60), None, None).await {
        Ok(output) => {
            println!("{}", output.text);
            match output.usage {
                Some(usage) => eprintln!("usage: in={} out={}", usage.input, usage.output),
                None => eprintln!("usage: UNKNOWN (provider did not report it)"),
            }
            if let Some(warning) = output.metering_warning {
                eprintln!("[warning] {warning}");
            }
        }
        Err(error) => eprintln!("[error] {error}"),
    }
}

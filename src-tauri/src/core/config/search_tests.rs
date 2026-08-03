use super::Config;

fn load_search_url(url: &str) -> Result<Config, String> {
    let root = std::env::temp_dir().join(format!("kxen-search-endpoint-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("config.toml");
    std::fs::write(&path, format!("[search]\nsearxng_url = {url:?}\n")).unwrap();
    let result = Config::load(&path, None).map_err(|error| error.to_string());
    std::fs::remove_dir_all(root).ok();
    result
}

#[test]
fn searxng_endpoint_uses_the_protected_transport_policy() {
    for url in ["http://search.example.com", "https://10.0.0.8", "https://169.254.169.254"] {
        let error = load_search_url(url).unwrap_err();
        assert!(error.contains("search.searxng_url"), "{url}: {error}");
    }
    for url in ["https://search.example.com", "http://localhost:8080", "http://127.0.0.1:8080"] {
        load_search_url(url).unwrap_or_else(|error| panic!("{url}: {error}"));
    }
}

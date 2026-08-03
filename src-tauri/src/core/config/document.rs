use super::Config;

/// 在替换用户级 config.toml 前验证完整 candidate，避免先落盘再由热重载发现跨字段错误。
pub fn validate_user_document(document: &toml::Table, source: &str) -> crate::core::Result<()> {
    let mut config: Config = toml::Value::Table(document.clone())
        .try_into()
        .map_err(|error| crate::core::Error::Custom(format!("config deserialize {source}: {error}")))?;
    config.seed_default_roles();
    config.validate(source)
}

/// voice.set_engine 的局部更新：覆盖 engine/fallback（空数组 = 清空降级链，
/// 前端两个调用点都显式传当前链）；locale 仅 Some 时覆盖；transcribe_model 等其他键保留。
pub fn merge_voice_engine(doc: &mut toml::Table, engine: &str, fallback: &[String], locale: Option<&str>) {
    let entry = doc.entry("voice").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !entry.is_table() {
        *entry = toml::Value::Table(toml::Table::new());
    }
    let Some(voice) = entry.as_table_mut() else { return };
    voice.insert("engine".into(), toml::Value::String(engine.into()));
    voice.insert("fallback".into(), toml::Value::Array(fallback.iter().map(|f| toml::Value::String(f.clone())).collect()));
    if let Some(locale) = locale {
        voice.insert("locale".into(), toml::Value::String(locale.into()));
    }
}

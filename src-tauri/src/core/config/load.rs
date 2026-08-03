use super::{Config, merge_config_toml, validate_project_keys};
use std::path::Path;

impl Config {
    pub fn load(user: &Path, project: Option<&Path>) -> crate::core::Result<Self> {
        let mut merged = toml::Value::Table(toml::Table::new());
        let mut sources = Vec::new();
        let sources_to_load = std::iter::once((user.to_path_buf(), false)).chain(project.map(|path| (path.to_path_buf(), true)));
        for (path, is_project) in sources_to_load {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(crate::core::Error::Custom(format!("config inspect {}: {error}", path.display()))),
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|error| crate::core::Error::Custom(format!("config read {}: {error}", path.display())))?;
            let parsed: toml::Value =
                toml::from_str(&text).map_err(|error| crate::core::Error::Custom(format!("config parse {}: {error}", path.display())))?;
            if is_project {
                validate_project_keys(&parsed, &path)?;
            }
            merge_config_toml(&mut merged, parsed);
            sources.push(path);
        }
        Self::from_merged(merged, &sources)
    }

    /// 使用尚未落盘的用户级 candidate 预加载最终配置。项目级 overlay 仍从对应
    /// Workspace 读取并执行同一套 key 边界、反序列化与跨字段校验。
    pub fn load_with_user_document(document: &toml::Table, user_source: &Path, project: Option<&Path>) -> crate::core::Result<Self> {
        let mut merged = toml::Value::Table(document.clone());
        let mut sources = vec![user_source.to_path_buf()];
        if let Some(path) = project {
            match std::fs::symlink_metadata(path) {
                Ok(_) => {
                    let text = std::fs::read_to_string(path)
                        .map_err(|error| crate::core::Error::Custom(format!("config read {}: {error}", path.display())))?;
                    let parsed: toml::Value = toml::from_str(&text)
                        .map_err(|error| crate::core::Error::Custom(format!("config parse {}: {error}", path.display())))?;
                    validate_project_keys(&parsed, path)?;
                    merge_config_toml(&mut merged, parsed);
                    sources.push(path.to_path_buf());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(crate::core::Error::Custom(format!("config inspect {}: {error}", path.display()))),
            }
        }
        Self::from_merged(merged, &sources)
    }

    fn from_merged(merged: toml::Value, sources: &[std::path::PathBuf]) -> crate::core::Result<Self> {
        let source = sources.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(" + ");
        let mut config: Config = merged.try_into().map_err(|error| {
            crate::core::Error::Custom(format!("config deserialize {}: {error}", if source.is_empty() { "defaults" } else { &source }))
        })?;
        config.seed_default_roles();
        config.validate(if source.is_empty() { "defaults" } else { &source })?;
        Ok(config)
    }
}

//! 全局 config RPC 的串行 read-modify-write 与原子替换。

static CONFIG_RMW_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// 进程内所有全局 config RPC 的唯一 RMW 入口。锁覆盖 read -> mutate -> tmp+rename，
/// 避免并发请求复用同一 tmp 或各自基于旧快照写回造成 lost update。
pub(super) fn update_toml<T>(path: &std::path::Path, update: impl FnOnce(&mut toml::Table) -> Result<T, String>) -> Result<T, String> {
    update_toml_then(path, update, || Ok(()))
}

/// 需要同步热换内存态的写入口使用此变体；`after_write` 仍在 config 锁内，
/// 防止较早请求晚到并用旧快照覆盖较新请求已经重建的内存态。
pub(super) fn update_toml_then<T>(
    path: &std::path::Path,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
    after_write: impl FnOnce() -> Result<(), String>,
) -> Result<T, String> {
    update_toml_staged(path, update, |_| Ok(()), |_| after_write())
}

/// 两阶段 config 写回：candidate 在磁盘提交前完整准备；publish 失败时 caller 必须
/// 已保持或恢复旧内存，本函数再在同一 RMW 锁内补偿原始磁盘文档。
pub(super) fn update_toml_staged<T, P>(
    path: &std::path::Path,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
    prepare: impl FnOnce(&toml::Table) -> Result<P, String>,
    publish: impl FnOnce(&mut P) -> Result<(), String>,
) -> Result<T, String> {
    let _guard = CONFIG_RMW_LOCK.lock().map_err(|_| "config RMW lock poisoned".to_string())?;
    let original = read_toml(path)?;
    let mut updated = original.clone();
    let result = update(&mut updated)?;
    let mut prepared = prepare(&updated)?;
    let durability_warning = write_toml(path, &updated)?;
    match publish(&mut prepared) {
        Ok(()) => {
            report_durability_warning(path, durability_warning);
            Ok(result)
        }
        Err(error) => match write_toml(path, &original) {
            Ok(rollback_warning) => {
                report_durability_warning(path, rollback_warning);
                Err(format!("config reload failed: {error}; config compensation: PASS"))
            }
            Err(rollback_error) => Err(format!("config reload failed: {error}; config compensation: FAIL: {rollback_error}")),
        },
    }
}

pub(super) fn update_toml_with_runtime<T>(
    path: &std::path::Path,
    runtimes: &kxen_app::workspace_runtime::WorkspaceRuntimeRegistry,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
) -> Result<T, String> {
    update_toml_staged(
        path,
        update,
        |candidate| runtimes.prepare_config_update(candidate),
        |runtime| {
            runtime.apply()?;
            runtime.commit();
            Ok(())
        },
    )
}

/// 跨存储事务先提交 config，再执行第二存储；第二步失败时在同一 config 锁内恢复原文档。
/// 返回值中的 PASS/FAIL 让上层 RPC 能区分业务失败与补偿失败。
#[cfg(test)]
pub(super) fn update_toml_transaction<T, U>(
    path: &std::path::Path,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
    after_write: impl FnOnce() -> Result<U, String>,
) -> Result<(T, U), String> {
    update_toml_transaction_prepared(path, |_| Ok(()), update, after_write)
}

#[cfg(test)]
pub(super) fn update_toml_transaction_prepared<T, U>(
    path: &std::path::Path,
    prepare: impl FnOnce(&toml::Table) -> Result<(), String>,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
    after_write: impl FnOnce() -> Result<U, String>,
) -> Result<(T, U), String> {
    let _guard = CONFIG_RMW_LOCK.lock().map_err(|_| "config RMW lock poisoned".to_string())?;
    let original = read_toml(path)?;
    prepare(&original)?;
    let mut updated = original.clone();
    let result = update(&mut updated)?;
    let durability_warning = write_toml(path, &updated)?;
    match after_write() {
        Ok(after_result) => {
            report_durability_warning(path, durability_warning);
            Ok((result, after_result))
        }
        Err(error) => match write_toml(path, &original) {
            Ok(rollback_warning) => {
                report_durability_warning(path, rollback_warning);
                Err(format!("second store update failed: {error}; config compensation: PASS"))
            }
            Err(rollback_error) => Err(format!("second store update failed: {error}; config compensation: FAIL: {rollback_error}")),
        },
    }
}

pub(super) fn update_toml_transaction_staged<T, U, P>(
    path: &std::path::Path,
    prepare_original: impl FnOnce(&toml::Table) -> Result<(), String>,
    update: impl FnOnce(&mut toml::Table) -> Result<T, String>,
    prepare_candidate: impl FnOnce(&toml::Table) -> Result<P, String>,
    after_write: impl FnOnce(&mut P) -> Result<U, String>,
) -> Result<(T, U, P), String> {
    let _guard = CONFIG_RMW_LOCK.lock().map_err(|_| "config RMW lock poisoned".to_string())?;
    let original = read_toml(path)?;
    prepare_original(&original)?;
    let mut updated = original.clone();
    let result = update(&mut updated)?;
    let mut prepared = prepare_candidate(&updated)?;
    let durability_warning = write_toml(path, &updated)?;
    match after_write(&mut prepared) {
        Ok(after_result) => {
            report_durability_warning(path, durability_warning);
            Ok((result, after_result, prepared))
        }
        Err(error) => match write_toml(path, &original) {
            Ok(rollback_warning) => {
                report_durability_warning(path, rollback_warning);
                Err(format!("second store update failed: {error}; config compensation: PASS"))
            }
            Err(rollback_error) => Err(format!("second store update failed: {error}; config compensation: FAIL: {rollback_error}")),
        },
    }
}

/// toml 1.x 文档读（Value::from_str 解析的是值不是文档）。
fn read_toml(path: &std::path::Path) -> Result<toml::Table, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("config read {}: {error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(toml::Table::new());
    }
    toml::from_str(&text).map_err(|error| format!("config parse {}: {error}", path.display()))
}

/// 原子且掉电可恢复地写回（tmp sync + rename + parent sync）。
fn write_toml(path: &std::path::Path, doc: &toml::Table) -> Result<Option<String>, String> {
    use std::io::Write;
    kxen_app::core::config::validate_user_document(doc, &path.display().to_string()).map_err(|error| error.to_string())?;
    let parent = path.parent().ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("config mkdir {}: {error}", parent.display()))?;
    let tmp = path.with_extension("toml.tmp");
    let text = toml::to_string(doc).map_err(|error| format!("config serialize {}: {error}", path.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("config open {}: {error}", tmp.display()))?;
    file.write_all(text.as_bytes()).map_err(|error| format!("config write {}: {error}", tmp.display()))?;
    file.sync_all().map_err(|error| format!("config sync {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("config replace {}: {error}", path.display())
    })?;
    #[cfg(unix)]
    return Ok(sync_config_directory(parent)
        .err()
        .map(|error| format!("config commit is visible but directory sync failed for {}: {error}", parent.display())));
    #[cfg(not(unix))]
    Ok(None)
}

#[cfg(unix)]
fn sync_config_directory(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_CONFIG_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected config directory sync failure"));
    }
    std::fs::File::open(path)?.sync_all()
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_CONFIG_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn report_durability_warning(path: &std::path::Path, warning: Option<String>) {
    if let Some(error) = warning {
        tracing::error!(config = %path.display(), %error, "config commit durability is indeterminate");
    }
}

#[cfg(test)]
mod tests;

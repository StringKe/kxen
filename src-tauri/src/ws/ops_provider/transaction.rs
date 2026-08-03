//! 自定义 provider 跨 config.toml/auth.json 的 crash-safe 补偿事务。

use kxen_app::auth::credential::{AuthStore, CredentialKind};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Prepared,
    Committed,
}

#[derive(Serialize, Deserialize)]
struct TransactionJournal {
    phase: JournalPhase,
    provider: String,
    original_config: toml::Table,
    original_auth: Vec<(String, CredentialKind)>,
}

pub(super) fn transact_custom_provider_with_runtime(
    config_path: &Path,
    auth_path: &Path,
    provider: &str,
    runtimes: &kxen_app::workspace_runtime::WorkspaceRuntimeRegistry,
    update_config: impl FnOnce(&mut toml::Table) -> Result<(), String>,
    update_auth: impl FnOnce(&mut AuthStore) -> Result<(), String>,
) -> Result<(AuthStore, Option<String>), String> {
    recover_custom_provider_transaction(config_path, auth_path)?;
    let result = super::super::ops_config::update_toml_transaction_staged(
        config_path,
        |original_config| {
            let auth = kxen_app::auth::credential::read_auth_file(auth_path).map_err(|error| error.to_string())?;
            let durability_warning = write_journal(
                config_path,
                &TransactionJournal {
                    phase: JournalPhase::Prepared,
                    provider: provider.to_string(),
                    original_config: original_config.clone(),
                    original_auth: custom_auth_entries(&auth, provider),
                },
            )?;
            if let Some(error) = durability_warning {
                let cleanup = match remove_journal(config_path) {
                    Ok(Some(cleanup)) | Err(cleanup) => format!("; cleanup: {cleanup}"),
                    Ok(None) => String::new(),
                };
                return Err(format!("prepared custom provider journal is not durable: {error}{cleanup}"));
            }
            Ok(())
        },
        update_config,
        |candidate| runtimes.prepare_config_update(candidate),
        |runtime| {
            let store = update_auth_with_compensation(auth_path, provider, update_auth)?;
            runtime.apply().map_err(|error| format!("runtime config apply failed: {error}"))?;
            Ok(store)
        },
    );

    let (_, store, mut runtime) = match result {
        Ok(result) => result,
        Err(error) => {
            return match recover_custom_provider_transaction(config_path, auth_path) {
                Ok(()) => Err(format!("{error}; crash journal recovery: PASS")),
                Err(recovery) => Err(format!("{error}; crash journal recovery: FAIL: {recovery}")),
            };
        }
    };

    let mut journal = read_journal(config_path)?.ok_or("custom provider transaction journal disappeared before commit")?;
    journal.phase = JournalPhase::Committed;
    match write_journal(config_path, &journal) {
        Ok(Some(error)) => {
            runtime.commit();
            return Ok((
                store,
                Some(format!(
                    "custom provider changes are visible but commit durability is indeterminate: {error}; restart recovery remains armed"
                )),
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(rollback_runtime_and_stores(
                &mut runtime,
                config_path,
                auth_path,
                format!("mark custom provider transaction committed: {error}"),
            ));
        }
    }
    runtime.commit();
    match remove_journal(config_path) {
        Ok(Some(error)) | Err(error) => {
            tracing::error!(%error, "durable committed custom provider journal cleanup deferred to startup");
        }
        Ok(None) => {}
    }
    Ok((store, None))
}

#[cfg(test)]
pub(super) fn transact_custom_provider(
    config_path: &Path,
    auth_path: &Path,
    provider: &str,
    update_config: impl FnOnce(&mut toml::Table) -> Result<(), String>,
    update_auth: impl FnOnce(&mut AuthStore) -> Result<(), String>,
) -> Result<(AuthStore, Option<String>), String> {
    let runtimes = kxen_app::workspace_runtime::WorkspaceRuntimeRegistry::with_user_config(config_path.to_path_buf())?;
    transact_custom_provider_with_runtime(config_path, auth_path, provider, &runtimes, update_config, update_auth)
}

fn rollback_runtime_and_stores(
    runtime: &mut kxen_app::workspace_runtime::RuntimeConfigUpdate,
    config_path: &Path,
    auth_path: &Path,
    prefix: String,
) -> String {
    let memory = runtime.rollback();
    let stores = recover_custom_provider_transaction(config_path, auth_path);
    match (memory, stores) {
        (Ok(()), Ok(())) => format!("{prefix}; runtime compensation: PASS; crash journal recovery: PASS"),
        (Err(memory), Ok(())) => format!("{prefix}; runtime compensation: FAIL: {memory}; crash journal recovery: PASS"),
        (Ok(()), Err(stores)) => format!("{prefix}; runtime compensation: PASS; crash journal recovery: FAIL: {stores}"),
        (Err(memory), Err(stores)) => {
            format!("{prefix}; runtime compensation: FAIL: {memory}; crash journal recovery: FAIL: {stores}")
        }
    }
}

pub(crate) fn recover_custom_provider_transaction(config_path: &Path, auth_path: &Path) -> Result<(), String> {
    let Some(journal) = read_journal(config_path)? else { return Ok(()) };
    if matches!(journal.phase, JournalPhase::Prepared) {
        super::super::ops_config::update_toml(config_path, |document| {
            *document = journal.original_config.clone();
            Ok(())
        })?;
        kxen_app::auth::credential::update_auth_file(auth_path, |disk| {
            for key in kxen_app::auth::credential::accounts_of(disk, &journal.provider) {
                disk.remove(&key);
            }
            disk.extend(journal.original_auth.clone());
            Ok(())
        })
        .map_err(|error| format!("restore custom provider auth: {error}"))?;
    }
    match remove_journal(config_path)? {
        Some(error) => Err(format!("custom provider journal removal is visible but durability is indeterminate: {error}")),
        None => Ok(()),
    }
}

fn update_auth_with_compensation(
    auth_path: &Path,
    provider: &str,
    update: impl FnOnce(&mut AuthStore) -> Result<(), String>,
) -> Result<AuthStore, String> {
    let mut baseline = None;
    let updated = kxen_app::auth::credential::update_auth_file(auth_path, |disk| {
        baseline = Some(custom_auth_entries(disk, provider));
        update(disk)
    });
    match updated {
        Ok(store) => Ok(store),
        Err(error) => {
            let Some(entries) = baseline else {
                return Err(format!("auth update failed before mutation: {error}; auth compensation: SKIP"));
            };
            match kxen_app::auth::credential::update_auth_file(auth_path, |disk| {
                for key in kxen_app::auth::credential::accounts_of(disk, provider) {
                    disk.remove(&key);
                }
                disk.extend(entries);
                Ok(())
            }) {
                Ok(_) => Err(format!("auth update failed: {error}; auth compensation: PASS")),
                Err(rollback_error) => Err(format!("auth update failed: {error}; auth compensation: FAIL: {rollback_error}")),
            }
        }
    }
}

fn custom_auth_entries(store: &AuthStore, provider: &str) -> Vec<(String, CredentialKind)> {
    kxen_app::auth::credential::accounts_of(store, provider)
        .into_iter()
        .filter_map(|key| store.get(&key).cloned().map(|credential| (key, credential)))
        .collect()
}

fn journal_path(config_path: &Path) -> Result<PathBuf, String> {
    let parent = config_path.parent().ok_or_else(|| format!("config path has no parent: {}", config_path.display()))?;
    Ok(parent.join("custom-provider.transaction.json"))
}

fn read_journal(config_path: &Path) -> Result<Option<TransactionJournal>, String> {
    let path = journal_path(config_path)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect custom provider journal {}: {error}", path.display())),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(format!("custom provider journal is not a regular file: {}", path.display()));
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("read custom provider journal {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| format!("parse custom provider journal {}: {error}", path.display()))
}

fn write_journal(config_path: &Path, journal: &TransactionJournal) -> Result<Option<String>, String> {
    use std::io::Write;
    #[cfg(test)]
    if FAIL_NEXT_JOURNAL_WRITE.with(|flag| flag.replace(false)) {
        return Err("injected custom provider journal write failure".into());
    }
    let path = journal_path(config_path)?;
    let parent = path.parent().ok_or_else(|| format!("journal path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create journal directory {}: {error}", parent.display()))?;
    let tmp = parent.join(format!(".custom-provider.transaction-{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec(journal).map_err(|error| format!("serialize custom provider journal: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|error| format!("create custom provider journal {}: {error}", tmp.display()))?;
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = result.and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = result {
        std::fs::remove_file(&tmp).ok();
        return Err(format!("commit custom provider journal {}: {error}", path.display()));
    }
    Ok(sync_directory(parent)
        .err()
        .map(|error| format!("custom provider journal commit is visible but directory sync failed for {}: {error}", parent.display())))
}

fn remove_journal(config_path: &Path) -> Result<Option<String>, String> {
    let path = journal_path(config_path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(path.parent().and_then(|parent| {
            sync_directory(parent).err().map(|error| format!("removed journal directory sync failed for {}: {error}", parent.display()))
        })),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("remove custom provider journal {}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_JOURNAL_DIRECTORY_SYNC.with(|flag| flag.replace(false)) {
        return Err(std::io::Error::other("injected custom provider journal directory sync failure"));
    }
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_JOURNAL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_JOURNAL_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests;

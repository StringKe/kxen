use super::{RecoveryManifest, read_manifest};
use std::path::{Path, PathBuf};

#[path = "storage/path.rs"]
mod path;
pub(super) use path::sync_dir;
use path::{
    canonical_bundle_exists, discard_backup_exists, nonce, recovery_source_exists, remove_file_if_exists, remove_path_if_exists,
    restore_target_exists, sync_dir_if_exists,
};

#[cfg(test)]
#[path = "storage/tests.rs"]
mod tests;

fn restore_lock(bundle: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>>> =
        std::sync::LazyLock::new(Default::default);
    crate::core::shared::lock(&LOCKS).entry(bundle.to_path_buf()).or_default().clone()
}

pub fn restore_storage(sessions_dir: &Path, team_root: &Path, bundle: &Path) -> Result<RecoveryManifest, String> {
    restore_storage_inner(sessions_dir, team_root, bundle, false)
}

/// 删除失败回滚使用严格模式：meta 已存在时也必须与 recovery bundle 的所有路径一致，
/// 不把 purge 后残留的冲突路径误报为恢复成功。
pub fn restore_storage_exact(sessions_dir: &Path, team_root: &Path, bundle: &Path) -> Result<RecoveryManifest, String> {
    restore_storage_inner(sessions_dir, team_root, bundle, true)
}

fn restore_storage_inner(sessions_dir: &Path, team_root: &Path, bundle: &Path, exact_existing: bool) -> Result<RecoveryManifest, String> {
    let lock = restore_lock(bundle);
    let _guard = crate::core::shared::lock(&lock);
    let manifest = read_manifest(bundle)?;
    let id = &manifest.session_id;
    let meta_source = bundle.join("session/meta.json");
    let meta_target = sessions_dir.join(format!("{id}.json"));
    std::fs::create_dir_all(sessions_dir).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(team_root).map_err(|error| error.to_string())?;

    if restore_target_exists(&meta_target)? {
        if !same_entry(&meta_source, &meta_target)? {
            return Err(format!("restore target already exists: {id}"));
        }
        install_missing_paths(sessions_dir, team_root, bundle, id, exact_existing)?;
        return Ok(manifest);
    }

    let mappings = restore_paths(sessions_dir, team_root, bundle, id);
    for (source, target) in &mappings {
        if restore_target_exists(target)? {
            let source_exists = recovery_source_exists(source)?;
            if !source_exists || !same_entry(source, target)? {
                return Err(format!("restore target already exists: {}", target.display()));
            }
        }
    }
    let mut installed = Vec::new();
    let result = (|| {
        for (source, target) in &mappings {
            if install_atomic(source, target)? {
                installed.push(target.clone());
            }
        }
        if install_atomic(&meta_source, &meta_target)? {
            installed.push(meta_target.clone());
        }
        Ok(())
    })();
    if let Err(error) = result {
        return Err(rollback_installed(installed, error));
    }
    Ok(manifest)
}

fn install_missing_paths(sessions_dir: &Path, team_root: &Path, bundle: &Path, id: &str, exact_existing: bool) -> Result<(), String> {
    let mappings = restore_paths(sessions_dir, team_root, bundle, id);
    for (source, target) in &mappings {
        if restore_target_exists(target)? {
            let source_exists = recovery_source_exists(source)?;
            if (source_exists && !same_entry(source, target)?) || (exact_existing && !source_exists) {
                return Err(format!("restore target differs from recovery bundle: {}", target.display()));
            }
        }
    }
    let mut installed = Vec::new();
    for (source, target) in mappings {
        match install_atomic(&source, &target) {
            Ok(true) => installed.push(target),
            Ok(false) => {}
            Err(error) => return Err(rollback_installed(installed, error)),
        }
    }
    Ok(())
}

fn restore_paths(sessions_dir: &Path, team_root: &Path, bundle: &Path, id: &str) -> Vec<(PathBuf, PathBuf)> {
    vec![
        (bundle.join("session/messages.jsonl"), sessions_dir.join(format!("{id}.jsonl"))),
        (bundle.join("session/compact.json"), sessions_dir.join(format!("{id}.compact.json"))),
        (bundle.join("session/queue.json"), sessions_dir.join(format!("{id}.queue.json"))),
        (bundle.join("session/artifacts"), sessions_dir.join(id)),
        (bundle.join("team"), team_root.join(id)),
    ]
}

fn install_atomic(source: &Path, target: &Path) -> Result<bool, String> {
    install_atomic_with(source, target, |_| Ok(()))
}

fn install_atomic_with(
    source: &Path,
    target: &Path,
    before_target_recheck: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<bool, String> {
    if !recovery_source_exists(source)? {
        return Ok(false);
    }
    if restore_target_exists(target)? {
        return Ok(false);
    }
    let parent = target.parent().ok_or_else(|| format!("restore target has no parent: {}", target.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let name = target.file_name().and_then(|name| name.to_str()).ok_or_else(|| "invalid restore target name".to_string())?;
    let temp = parent.join(format!(".{name}.restore-{}-{}", std::process::id(), nonce()));
    remove_path_if_exists(&temp)?;
    if let Err(error) = copy_required(source, &temp).and_then(|()| sync_tree(&temp)) {
        return Err(cleanup_restore_temp(&temp, error));
    }
    let target_exists =
        before_target_recheck(target).and_then(|()| restore_target_exists(target)).map_err(|error| cleanup_restore_temp(&temp, error))?;
    if target_exists {
        let same = same_entry(source, target).map_err(|error| cleanup_restore_temp(&temp, error))?;
        remove_path_if_exists(&temp)?;
        return if same { Ok(false) } else { Err(format!("restore target appeared concurrently: {}", target.display())) };
    }
    if let Err(error) = std::fs::rename(&temp, target) {
        let cleanup = remove_path_if_exists(&temp).err();
        return Err(cleanup.map_or_else(
            || format!("commit restore {}: {error}", target.display()),
            |cleanup| format!("commit restore {}: {error}; temp cleanup failed: {cleanup}", target.display()),
        ));
    }
    sync_dir(parent)?;
    Ok(true)
}

fn cleanup_restore_temp(temp: &Path, cause: String) -> String {
    remove_path_if_exists(temp).err().map_or(cause.clone(), |cleanup| format!("{cause}; restore temp cleanup failed: {cleanup}"))
}

fn rollback_installed(installed: Vec<PathBuf>, cause: String) -> String {
    let errors: Vec<String> = installed.into_iter().rev().filter_map(|path| remove_path_if_exists(&path).err()).collect();
    if errors.is_empty() { cause } else { format!("{cause}; restore rollback failed: {}", errors.join("; ")) }
}

pub fn purge_storage(
    sessions_dir: &Path,
    team_root: &Path,
    session_id: &str,
    transaction: &super::DeletionTransaction,
) -> Result<(), String> {
    crate::core::ids::validate_id(session_id).map_err(|error| error.to_string())?;
    transaction.validate(sessions_dir, session_id)?;
    let mut errors = Vec::new();
    for path in [
        sessions_dir.join(format!("{session_id}.jsonl")),
        sessions_dir.join(format!("{session_id}.compact.json")),
        sessions_dir.join(format!("{session_id}.queue.json")),
    ] {
        if let Err(error) = remove_file_if_exists(&path) {
            errors.push(error);
        }
    }
    for path in [sessions_dir.join(session_id), team_root.join(session_id)] {
        if let Err(error) = remove_path_if_exists(&path) {
            errors.push(error);
        }
    }
    if errors.is_empty()
        && let Err(error) = remove_file_if_exists(&sessions_dir.join(format!("{session_id}.json")))
    {
        errors.push(error);
    }
    if errors.is_empty() {
        sync_dir_if_exists(sessions_dir)?;
        sync_dir_if_exists(team_root)
    } else {
        Err(errors.join("; "))
    }
}

pub fn discard_bundle(bundle: &Path) -> Result<(), String> {
    let temporary = bundle.starts_with(std::env::temp_dir());
    discard_bundle_with(bundle, move |canonical| {
        if temporary {
            std::fs::remove_dir_all(canonical).map_err(|error| error.to_string())
        } else {
            trash::delete(canonical).map_err(|error| error.to_string())
        }
    })
}

pub(crate) fn discard_bundle_with(bundle: &Path, discard: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let parent = bundle.parent().ok_or_else(|| format!("recovery bundle has no parent: {}", bundle.display()))?;
    let name = bundle.file_name().ok_or_else(|| "recovery bundle has no name".to_string())?;
    let backup_dir = parent.join(".backup");
    let backup = backup_dir.join(name);
    std::fs::create_dir_all(&backup_dir).map_err(|error| error.to_string())?;
    if discard_backup_exists(&backup)? {
        return Err(format!("recovery discard backup already exists: {}", backup.display()));
    }
    if let Err(error) = copy_required(bundle, &backup) {
        let cleanup = remove_path_if_exists(&backup).err();
        return Err(cleanup.map_or(error.clone(), |cleanup| format!("{error}; transfer cleanup failed: {cleanup}")));
    }
    if let Err(error) = sync_tree(&backup).and_then(|()| sync_dir(&backup_dir)) {
        let cleanup = remove_path_if_exists(&backup).err();
        return Err(cleanup.map_or(error.clone(), |cleanup| format!("{error}; backup cleanup failed: {cleanup}")));
    }
    if let Err(error) = discard(bundle) {
        let recovery = recover_discard_backup(bundle).err();
        return Err(recovery.map_or(error.clone(), |recovery| format!("{error}; local recovery failed: {recovery}")));
    }
    if canonical_bundle_exists(bundle)? {
        recover_discard_backup(bundle)?;
        return Err(format!("discard reported success but recovery bundle still exists: {}", bundle.display()));
    }
    remove_path_if_exists(&backup)?;
    sync_dir(&backup_dir)?;
    sync_dir(parent)
}

/// Trash 操作报错或进程崩溃时，用本地 backup 恢复 canonical bundle。
/// canonical 已存在说明 Trash 未消费它，此时只移除重复 backup。
pub fn recover_discard_backup(bundle: &Path) -> Result<bool, String> {
    let parent = bundle.parent().ok_or_else(|| format!("recovery bundle has no parent: {}", bundle.display()))?;
    let name = bundle.file_name().ok_or_else(|| "recovery bundle has no name".to_string())?;
    let backup_dir = parent.join(".backup");
    let backup = backup_dir.join(name);
    let backup_exists = discard_backup_exists(&backup)?;
    let bundle_exists = canonical_bundle_exists(bundle)?;
    if !backup_exists {
        return Ok(bundle_exists);
    }
    if bundle_exists {
        if !same_entry(&backup, bundle)? {
            return Err(format!("canonical recovery bundle conflicts with discard backup: {}", bundle.display()));
        }
        remove_path_if_exists(&backup)?;
    } else {
        std::fs::rename(&backup, bundle).map_err(|error| format!("restore discard backup {}: {error}", bundle.display()))?;
        sync_tree(bundle)?;
    }
    sync_dir(&backup_dir)?;
    sync_dir(parent)?;
    Ok(true)
}

pub fn complete_restore(bundle: &Path) -> Result<(), String> {
    remove_path_if_exists(bundle)?;
    bundle.parent().map(sync_dir).unwrap_or(Ok(()))
}

pub(super) fn copy_required(source: &Path, target: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(source) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(format!("recovery source symlink refused: {}", source.display()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("recovery source missing: {}", source.display()));
        }
        Err(error) => return Err(error.to_string()),
    }
    copy_optional(source, target)
}

pub(super) fn copy_optional(source: &Path, target: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!("recovery source symlink refused: {}", source.display()));
    }
    if metadata.is_dir() {
        std::fs::create_dir_all(target).map_err(|error| error.to_string())?;
        for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            copy_optional(&entry.path(), &target.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    std::fs::copy(source, target).map(|_| ()).map_err(|error| error.to_string())
}

fn same_entry(left: &Path, right: &Path) -> Result<bool, String> {
    let left_meta = std::fs::symlink_metadata(left).map_err(|error| error.to_string())?;
    let right_meta = std::fs::symlink_metadata(right).map_err(|error| error.to_string())?;
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() || left_meta.is_dir() != right_meta.is_dir() {
        return Ok(false);
    }
    if !left_meta.is_dir() {
        return Ok(std::fs::read(left).map_err(|error| error.to_string())? == std::fs::read(right).map_err(|error| error.to_string())?);
    }
    let mut left_names: Vec<_> = std::fs::read_dir(left)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(|error| error.to_string()))
        .collect::<Result<_, _>>()?;
    let mut right_names: Vec<_> = std::fs::read_dir(right)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(|error| error.to_string()))
        .collect::<Result<_, _>>()?;
    left_names.sort();
    right_names.sort();
    if left_names != right_names {
        return Ok(false);
    }
    for name in left_names {
        if !same_entry(&left.join(&name), &right.join(name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn sync_tree(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|error| format!("read {}: {error}", path.display()))? {
            let entry = entry.map_err(|error| error.to_string())?;
            sync_tree(&entry.path())?;
        }
        return sync_dir(path);
    }
    std::fs::File::open(path).and_then(|file| file.sync_all()).map_err(|error| format!("sync {}: {error}", path.display()))
}

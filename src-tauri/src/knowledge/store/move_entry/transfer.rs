use super::super::path_lock;
use super::claim::{MoveClaim, persist_claim};
use super::path::{ensure_safe_parent, path_present, sync_move_directory};
use sha2::Digest;
use std::io::Read;
use std::path::Path;

pub(super) fn execute_claim(path: &Path, claim: &MoveClaim, force_copy: bool) -> Result<String, String> {
    let source_lock = path_lock(&claim.source);
    let destination_lock = path_lock(&claim.destination);
    let (_source_guard, _destination_guard) = if claim.source <= claim.destination {
        (source_lock.lock().map_err(|error| error.to_string())?, destination_lock.lock().map_err(|error| error.to_string())?)
    } else {
        let destination_guard = destination_lock.lock().map_err(|error| error.to_string())?;
        let source_guard = source_lock.lock().map_err(|error| error.to_string())?;
        (source_guard, destination_guard)
    };

    match (path_present(&claim.source)?, path_present(&claim.destination)?) {
        (false, true) => return finish_claim(path, claim),
        (false, false) => return Err("knowledge move is BLOCKED: both source and destination are missing".into()),
        (true, true) => {
            if !equivalent(&claim.source, &claim.destination)? {
                return Err("knowledge move is BLOCKED: source and destination differ".into());
            }
            trash_source(&claim.source)?;
            return finish_claim(path, claim);
        }
        (true, false) => {}
    }

    let parent = claim.destination.parent().ok_or("destination has no parent")?;
    ensure_safe_parent(&claim.destination_root, parent, true)?;
    let renamed = if force_copy {
        false
    } else {
        match std::fs::rename(&claim.source, &claim.destination) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => false,
            Err(error) => return Err(format!("move {}: {error}", claim.source.display())),
        }
    };
    if renamed {
        sync_visible_move(claim)?;
    } else {
        if path_present(&claim.staging)? {
            if equivalent(&claim.source, &claim.staging)? {
                std::fs::rename(&claim.staging, &claim.destination)
                    .map_err(|error| format!("publish recovered knowledge move {}: {error}", claim.destination.display()))?;
            } else {
                trash::delete(&claim.staging)
                    .map_err(|error| format!("discard stale move staging {}: {error}", claim.staging.display()))?;
                sync_move_directory(parent)?;
                copy_tree(&claim.source, &claim.staging)?;
                std::fs::rename(&claim.staging, &claim.destination)
                    .map_err(|error| format!("publish knowledge move {}: {error}", claim.destination.display()))?;
            }
        } else {
            copy_tree(&claim.source, &claim.staging)?;
            std::fs::rename(&claim.staging, &claim.destination)
                .map_err(|error| format!("publish knowledge move {}: {error}", claim.destination.display()))?;
        }
        sync_move_directory(parent)?;
        #[cfg(test)]
        if FAIL_AFTER_PUBLISH.with(|flag| flag.replace(false)) {
            return Err("injected failure after knowledge move publish".into());
        }
        trash_source(&claim.source)?;
    }
    finish_claim(path, claim)
}

fn trash_source(source: &Path) -> Result<(), String> {
    let parent = source.parent().ok_or("source has no parent")?;
    trash::delete(source).map_err(|error| format!("move source to Trash {}: {error}", source.display()))?;
    sync_move_directory(parent)
}

fn sync_visible_move(claim: &MoveClaim) -> Result<(), String> {
    let destination_parent = claim.destination.parent().ok_or("destination has no parent")?;
    sync_move_directory(destination_parent)?;
    let source_parent = claim.source.parent().ok_or("source has no parent")?;
    if source_parent != destination_parent {
        sync_move_directory(source_parent)?;
    }
    Ok(())
}

fn finish_claim(path: &Path, claim: &MoveClaim) -> Result<String, String> {
    if !path_present(&claim.destination)? {
        return Err("knowledge move destination is not visible".into());
    }
    if path_present(&claim.source)? {
        return Err("knowledge move source is still visible".into());
    }
    // 任何重试状态都先重新同步 source/destination parents，再清 claim。之前一次
    // rename/trash 后的 fsync 失败不能因可见路径形态正确而被错误当成 durable complete。
    sync_visible_move(claim)?;
    match std::fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent()
                && let Err(error) = sync_move_directory(parent)
            {
                // remove 已可见但 parent fsync 失败时，立即恢复 private claim，确保
                // 同进程 retry 仍可根据 source-absent/destination-present 完成恢复。
                return match persist_claim(path, claim) {
                    Ok(()) => Err(format!("knowledge move claim removal durability is indeterminate: {error}; claim restored for retry")),
                    Err(repair) => {
                        Err(format!("knowledge move claim removal durability is BLOCKED: {error}; claim restore failed: {repair}"))
                    }
                };
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove knowledge move claim {}: {error}", path.display())),
    }
    Ok(claim.destination.to_string_lossy().into_owned())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(source).map_err(|error| format!("inspect {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("cross-filesystem knowledge move rejects symlink: {}", source.display()));
    }
    if metadata.is_file() {
        let mut input = std::fs::File::open(source).map_err(|error| format!("open {}: {error}", source.display()))?;
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|error| format!("create {}: {error}", destination.display()))?;
        std::io::copy(&mut input, &mut output).map_err(|error| format!("copy {}: {error}", source.display()))?;
        std::fs::set_permissions(destination, metadata.permissions())
            .map_err(|error| format!("chmod {}: {error}", destination.display()))?;
        output.sync_all().map_err(|error| format!("sync {}: {error}", destination.display()))?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(format!("unsupported knowledge entry type: {}", source.display()));
    }
    std::fs::create_dir(destination).map_err(|error| format!("create {}: {error}", destination.display()))?;
    std::fs::set_permissions(destination, metadata.permissions()).map_err(|error| format!("chmod {}: {error}", destination.display()))?;
    let mut entries = std::fs::read_dir(source)
        .map_err(|error| format!("read {}: {error}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {}: {error}", source.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
    }
    sync_move_directory(destination)
}

fn equivalent(left: &Path, right: &Path) -> Result<bool, String> {
    fn digest(path: &Path) -> Result<[u8; 32], String> {
        let mut file = std::fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let count = file.read(&mut buffer).map_err(|error| format!("read {}: {error}", path.display()))?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Ok(hasher.finalize().into())
    }
    let left_meta = std::fs::symlink_metadata(left).map_err(|error| error.to_string())?;
    let right_meta = std::fs::symlink_metadata(right).map_err(|error| error.to_string())?;
    if left_meta.file_type().is_symlink() || right_meta.file_type().is_symlink() {
        return Err("knowledge move recovery rejects symlinks".into());
    }
    if left_meta.is_file() && right_meta.is_file() {
        return Ok(left_meta.len() == right_meta.len() && digest(left)? == digest(right)?);
    }
    if !left_meta.is_dir() || !right_meta.is_dir() {
        return Ok(false);
    }
    let names = |path: &Path| -> Result<Vec<std::ffi::OsString>, String> {
        let mut names = std::fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .map(|entry| entry.map(|entry| entry.file_name()).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        names.sort();
        Ok(names)
    };
    let left_names = names(left)?;
    if left_names != names(right)? {
        return Ok(false);
    }
    for name in left_names {
        if !equivalent(&left.join(&name), &right.join(name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
thread_local! {
    static FAIL_AFTER_PUBLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn fail_after_publish() {
    FAIL_AFTER_PUBLISH.with(|flag| flag.set(true));
}

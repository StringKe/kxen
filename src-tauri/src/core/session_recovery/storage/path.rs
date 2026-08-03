use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) fn nonce() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub(super) fn restore_target_exists(target: &Path) -> Result<bool, String> {
    checked_exists(target, "restore target")
}

pub(super) fn recovery_source_exists(source: &Path) -> Result<bool, String> {
    checked_exists(source, "recovery source")
}

pub(super) fn discard_backup_exists(backup: &Path) -> Result<bool, String> {
    checked_exists(backup, "discard backup")
}

pub(super) fn canonical_bundle_exists(bundle: &Path) -> Result<bool, String> {
    checked_exists(bundle, "canonical recovery bundle")
}

fn checked_exists(path: &Path, role: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!("{role} symlink refused: {}", path.display())),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {role} {}: {error}", path.display())),
    }
}

pub(super) fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

pub(super) fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("inspect {}: {error}", path.display())),
    };
    let result = if metadata.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    result.map_err(|error| format!("remove {}: {error}", path.display()))
}

pub(super) fn sync_dir_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(format!("sync directory expected a directory: {}", path.display()))
        }
        Ok(_) => sync_dir(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect sync directory {}: {error}", path.display())),
    }
}

#[cfg(unix)]
pub(in crate::core::session_recovery) fn sync_dir(path: &Path) -> Result<(), String> {
    std::fs::File::open(path).and_then(|file| file.sync_all()).map_err(|error| format!("sync directory {}: {error}", path.display()))
}

#[cfg(not(unix))]
pub(in crate::core::session_recovery) fn sync_dir(_path: &Path) -> Result<(), String> {
    Ok(())
}

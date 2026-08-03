use crate::knowledge::Scope;
use std::path::{Component, Path, PathBuf};

pub(super) fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("inspect {label} {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} is not a real directory: {}", path.display()));
    }
    path.canonicalize().map_err(|error| format!("canonicalize {label} {}: {error}", path.display()))
}

pub(super) fn canonical_scope_root(scope: Scope, workspace: &Path, home: &Path, create: bool) -> Result<PathBuf, String> {
    let root = match scope {
        Scope::Project => workspace.join(".agents"),
        Scope::Personal => home.join(".agents"),
    };
    let existed = root.is_dir();
    if create {
        std::fs::create_dir_all(&root).map_err(|error| format!("create knowledge root {}: {error}", root.display()))?;
    }
    let metadata = std::fs::symlink_metadata(&root).map_err(|error| format!("inspect knowledge root {}: {error}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("knowledge root is not a real directory: {}", root.display()));
    }
    let canonical = root.canonicalize().map_err(|error| format!("canonicalize knowledge root {}: {error}", root.display()))?;
    let base = match scope {
        Scope::Project => workspace,
        Scope::Personal => home,
    };
    if !canonical.starts_with(base) {
        return Err(format!("knowledge root escapes its scope: {}", canonical.display()));
    }
    if create && !existed {
        sync_move_directory(base)?;
    }
    Ok(canonical)
}

pub(super) fn prepare_private_claim_root(root: &Path) -> Result<PathBuf, String> {
    let existed = root.is_dir();
    std::fs::create_dir_all(root).map_err(|error| format!("create knowledge move claim root {}: {error}", root.display()))?;
    let metadata =
        std::fs::symlink_metadata(root).map_err(|error| format!("inspect knowledge move claim root {}: {error}", root.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("knowledge move claim root is not a real directory: {}", root.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("secure knowledge move claim root {}: {error}", root.display()))?;
    }
    let canonical = root.canonicalize().map_err(|error| format!("canonicalize knowledge move claim root {}: {error}", root.display()))?;
    sync_move_directory(&canonical)?;
    if !existed {
        let parent = canonical.parent().ok_or_else(|| format!("knowledge move claim root has no parent: {}", canonical.display()))?;
        sync_move_directory(parent)?;
    }
    Ok(canonical)
}

pub(super) fn validate_relative(relative: &Path) -> Result<(), String> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("knowledge move relative path is invalid: {}", relative.display()));
    }
    Ok(())
}

pub(super) fn staging_path(destination: &Path, transaction_id: &str) -> Result<PathBuf, String> {
    let file_name = destination.file_name().and_then(|name| name.to_str()).ok_or("destination has no UTF-8 name")?;
    Ok(destination.with_file_name(format!(".{file_name}.{transaction_id}.kxen-move")))
}

pub(super) fn ensure_safe_parent(root: &Path, parent: &Path, create: bool) -> Result<(), String> {
    let relative = parent.strip_prefix(root).map_err(|_| format!("knowledge move parent escapes scope root: {}", parent.display()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!("knowledge move parent is invalid: {}", parent.display()));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(format!("knowledge move parent contains a symlink or non-directory: {}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                std::fs::create_dir(&current).map_err(|error| format!("create {}: {error}", current.display()))?;
                if let Some(parent) = current.parent() {
                    sync_move_directory(parent)?;
                }
            }
            Err(error) => return Err(format!("inspect {}: {error}", current.display())),
        }
    }
    Ok(())
}

pub(super) fn path_present(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!("knowledge move rejects symlink: {}", path.display())),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("inspect {}: {error}", path.display())),
    }
}

pub(super) fn reject_symlink_tree(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("knowledge move rejects symlink: {}", path.display()));
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|error| format!("read {}: {error}", path.display()))? {
            reject_symlink_tree(&entry.map_err(|error| format!("read {}: {error}", path.display()))?.path())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn sync_move_directory(path: &Path) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_SYNC_PATH.with(|target| target.borrow().as_ref().is_some_and(|target| target == path)) {
        FAIL_SYNC_PATH.with(|target| target.borrow_mut().take());
        return Err(format!("injected knowledge move directory sync failure: {}", path.display()));
    }
    std::fs::File::open(path).and_then(|directory| directory.sync_all()).map_err(|error| format!("sync {}: {error}", path.display()))
}

#[cfg(not(unix))]
pub(super) fn sync_move_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static FAIL_SYNC_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn fail_next_sync(path: &Path) {
    FAIL_SYNC_PATH.with(|target| *target.borrow_mut() = Some(path.to_path_buf()));
}

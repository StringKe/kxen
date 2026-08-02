//! Agent tool path boundary.
//!
//! Every model-controlled filesystem path is resolved here before it reaches a
//! file, search, LSP, shell, or background-task implementation. The boundary is
//! the canonical Workspace root plus explicit paths selected through the native
//! picker for the current Session. Credential locations are never grantable.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct ResolvedPath {
    absolute: PathBuf,
    authority_root: PathBuf,
    relative: PathBuf,
    authority: Arc<cap_std::fs::Dir>,
}

impl std::fmt::Debug for ResolvedPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedPath")
            .field("absolute", &self.absolute)
            .field("authority_root", &self.authority_root)
            .field("relative", &self.relative)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ResolvedPath {
    fn eq(&self, other: &Self) -> bool {
        self.absolute == other.absolute && self.authority_root == other.authority_root && self.relative == other.relative
    }
}

impl Eq for ResolvedPath {}

impl ResolvedPath {
    pub fn as_path(&self) -> &Path {
        &self.absolute
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.absolute
    }

    pub fn metadata(&self) -> std::io::Result<cap_std::fs::Metadata> {
        self.authority.metadata(&self.relative)
    }

    pub fn open(&self) -> std::io::Result<cap_std::fs::File> {
        self.authority.open(&self.relative)
    }

    pub fn read_dir(&self) -> std::io::Result<cap_std::fs::ReadDir> {
        self.authority.read_dir(&self.relative)
    }

    pub fn read_to_string(&self) -> std::io::Result<String> {
        let mut file = self.authority.open(&self.relative)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    }

    pub fn read_optional(&self) -> std::io::Result<Option<String>> {
        match self.read_to_string() {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 同一 capability 内写临时文件再 rename，目录句柄把所有路径解析限制在已授权 root。
    pub fn write_atomic(&self, bytes: &[u8]) -> std::io::Result<()> {
        let parent = self.relative.parent().unwrap_or_else(|| Path::new(""));
        self.authority.create_dir_all(parent)?;
        let parent_dir = self.authority.open_dir(parent)?;
        let file_name =
            self.relative.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "target has no file name"))?;
        let temporary = format!(".kxen-write-{}.tmp", uuid::Uuid::new_v4());
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = parent_dir.open_with(&temporary, &options)?;
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            parent_dir.remove_file(&temporary).ok();
            return Err(error);
        }
        drop(file);
        if let Err(error) = parent_dir.rename(&temporary, &parent_dir, file_name) {
            parent_dir.remove_file(&temporary).ok();
            return Err(error);
        }
        parent_dir.try_clone()?.into_std_file().sync_all()
    }

    /// 先用 anchored rename 把目标原子移到随机同根暂存名，再交给系统 Trash。
    /// 最终 path API 只看到已隔离的随机名，攻击者无法把原始 leaf 换成 Workspace 外目标。
    pub fn move_to_trash(&self) -> Result<(), String> {
        let file_name = self.relative.file_name().ok_or_else(|| "cannot trash authority root".to_string())?;
        let staged_name = format!(".{}.kxen-trash-{}", file_name.to_string_lossy(), uuid::Uuid::new_v4());
        let staged_relative = self.relative.with_file_name(staged_name);
        self.authority
            .rename(&self.relative, &self.authority, &staged_relative)
            .map_err(|error| format!("stage {}: {error}", self.absolute.display()))?;
        if let Err(error) = verify_open_root(&self.authority_root, &self.authority) {
            self.authority.rename(&staged_relative, &self.authority, &self.relative).ok();
            return Err(format!("authority root changed before trash: {error}"));
        }
        let staged_absolute = self.authority_root.join(&staged_relative);
        if let Err(error) = trash::delete(&staged_absolute) {
            let rollback = self.authority.rename(&staged_relative, &self.authority, &self.relative);
            return match rollback {
                Ok(()) => Err(format!("trash {}: {error}", self.absolute.display())),
                Err(rollback_error) => {
                    Err(format!("trash {}: {error}; staged item recovery failed: {rollback_error}", self.absolute.display()))
                }
            };
        }
        Ok(())
    }
}

/// Resolve a model-provided path against a Workspace and enforce the host
/// boundary. Nonexistent write targets are resolved through their nearest
/// existing ancestor so `..` and symlink escapes cannot hide in new paths.
pub fn resolve(input: &str, workspace: &Path, grants: &HashSet<PathBuf>) -> Result<ResolvedPath, String> {
    let workspace = canonicalize_existing(workspace).map_err(|e| format!("workspace path unavailable: {e}"))?;
    let expanded = expand_home(input)?;
    let candidate = if expanded.is_absolute() { expanded } else { workspace.join(expanded) };
    let candidate = canonicalize_lenient(&candidate)?;

    if let Some(reason) = sensitive_reason(&candidate) {
        return Err(format!("path denied: {reason}"));
    }
    if let crate::tools::safety::Verdict::Deny { rule_id, reason, .. } =
        crate::tools::safety::guard_path(&candidate.to_string_lossy(), &workspace.to_string_lossy())
    {
        return Err(format!("path denied by {rule_id}: {reason}"));
    }
    let authority_root = if candidate.starts_with(&workspace) {
        workspace.clone()
    } else {
        grant_root(&candidate, grants)
            .ok_or_else(|| format!("path escapes workspace: {} (workspace: {})", candidate.display(), workspace.display()))?
    };
    let relative = candidate
        .strip_prefix(&authority_root)
        .map_err(|_| format!("path is outside authority root: {}", candidate.display()))?
        .to_path_buf();
    let authority = open_verified_root(&authority_root)?;
    Ok(ResolvedPath { absolute: candidate, authority_root, relative, authority: Arc::new(authority) })
}

/// Canonicalize a path that may not exist yet. Existing ancestors are resolved
/// through the filesystem, then missing normal components are appended.
pub fn canonicalize_lenient(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().map_err(|e| e.to_string())?.join(path) };
    let normalized = lexical_normalize(&absolute)?;
    let mut cursor = normalized.as_path();
    let mut missing = Vec::new();
    while !cursor.exists() {
        let name = cursor.file_name().ok_or_else(|| format!("path has no existing ancestor: {}", path.display()))?;
        missing.push(name.to_os_string());
        cursor = cursor.parent().ok_or_else(|| format!("path has no existing ancestor: {}", path.display()))?;
    }
    let mut resolved = canonicalize_existing(cursor).map_err(|e| format!("canonicalize {}: {e}", cursor.display()))?;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn canonicalize_existing(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(format!("path escapes filesystem root: {}", path.display()));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn expand_home(input: &str) -> Result<PathBuf, String> {
    if input == "~" {
        return dirs::home_dir().ok_or("home directory unavailable".into());
    }
    if let Some(rest) = input.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(rest)).ok_or("home directory unavailable".into());
    }
    Ok(PathBuf::from(input))
}

fn grant_root(candidate: &Path, grants: &HashSet<PathBuf>) -> Option<PathBuf> {
    grants.iter().find_map(|grant| {
        let Ok(grant) = canonicalize_lenient(grant) else {
            return None;
        };
        if grant.is_dir() && candidate.starts_with(&grant) {
            Some(grant)
        } else if candidate == grant {
            grant.parent().map(Path::to_path_buf)
        } else {
            None
        }
    })
}

fn open_verified_root(root: &Path) -> Result<cap_std::fs::Dir, String> {
    let authority = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|error| format!("open authority root {}: {error}", root.display()))?;
    verify_open_root(root, &authority)?;
    Ok(authority)
}

fn verify_open_root(root: &Path, authority: &cap_std::fs::Dir) -> Result<(), String> {
    let path_metadata = std::fs::metadata(root).map_err(|error| format!("stat {}: {error}", root.display()))?;
    let handle_metadata = authority.metadata(".").map_err(|error| format!("stat open root {}: {error}", root.display()))?;
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as CapMetadataExt;
        use std::os::unix::fs::MetadataExt as StdMetadataExt;
        if StdMetadataExt::dev(&path_metadata) != CapMetadataExt::dev(&handle_metadata)
            || StdMetadataExt::ino(&path_metadata) != CapMetadataExt::ino(&handle_metadata)
        {
            return Err(format!("authority root identity changed: {}", root.display()));
        }
    }
    Ok(())
}

fn sensitive_reason(candidate: &Path) -> Option<String> {
    let home = dirs::home_dir()?;
    let mut roots = vec![
        crate::core::paths::config_dir(),
        crate::core::paths::data_dir(),
        crate::core::paths::cache_dir(),
        crate::core::paths::auth_file(),
        crate::mcp::oauth_store::store_path(),
        home.join("Library/Keychains"),
    ];
    for name in [".ssh", ".gnupg", ".aws", ".kube", ".docker", ".codex", ".claude", ".grok", ".kimi-code"] {
        roots.push(home.join(name));
    }
    if roots.iter().any(|root| same_or_descendant(candidate, root)) {
        return Some(format!("credential or application data is protected: {}", candidate.display()));
    }

    let file_name = candidate.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if [".netrc", ".npmrc", ".pypirc", ".git-credentials"].contains(&file_name) {
        return Some(format!("credential file is protected: {}", candidate.display()));
    }
    let extension = candidate.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase();
    if ["p8", "p12", "pfx", "keychain", "keychain-db"].contains(&extension.as_str()) {
        return Some(format!("private key or keychain file is protected: {}", candidate.display()));
    }
    None
}

fn same_or_descendant(candidate: &Path, root: &Path) -> bool {
    let root = canonicalize_lenient(root).unwrap_or_else(|_| root.to_path_buf());
    candidate == root || candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kxen-path-policy-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn contains_relative_absolute_missing_and_symlink_paths() {
        let work = workspace("contains");
        let inside = work.join("src/main.rs");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&inside, "fn main() {}\n").unwrap();
        assert_eq!(resolve("src/main.rs", &work, &HashSet::new()).unwrap().as_path(), inside.canonicalize().unwrap());
        assert_eq!(resolve(inside.to_str().unwrap(), &work, &HashSet::new()).unwrap().as_path(), inside.canonicalize().unwrap());
        assert!(resolve("new/deep/file.rs", &work, &HashSet::new()).unwrap().as_path().starts_with(work.canonicalize().unwrap()));

        let outside = workspace("outside");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, work.join("escape")).unwrap();
        assert!(resolve("escape/secret.txt", &work, &HashSet::new()).unwrap_err().contains("escapes workspace"));
        assert!(resolve("../outside/secret.txt", &work, &HashSet::new()).is_err());
        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    fn picker_grants_file_or_directory_but_never_credentials() {
        let work = workspace("grant-work");
        let outside = workspace("grant-outside");
        let file = outside.join("picked.txt");
        std::fs::write(&file, "ok").unwrap();
        let grants = HashSet::from([file.canonicalize().unwrap()]);
        assert!(resolve(file.to_str().unwrap(), &work, &grants).is_ok());
        assert!(resolve(outside.join("other.txt").to_str().unwrap(), &work, &grants).is_err());

        let dir_grants = HashSet::from([outside.canonicalize().unwrap()]);
        assert!(resolve(outside.join("new.txt").to_str().unwrap(), &work, &dir_grants).is_ok());
        let auth = crate::core::paths::auth_file();
        let auth_grants = HashSet::from([auth.clone()]);
        assert!(resolve(auth.to_str().unwrap(), &work, &auth_grants).unwrap_err().contains("protected"));
        std::fs::remove_dir_all(&work).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}

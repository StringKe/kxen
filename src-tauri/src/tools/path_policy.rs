//! Agent tool path boundary.
//!
//! Every model-controlled filesystem path is resolved here before it reaches a
//! file, search, LSP, shell, or background-task implementation. The boundary is
//! the canonical Workspace root plus explicit paths selected through the native
//! picker for the current Session. Credential locations are never grantable.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath(PathBuf);

impl ResolvedPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
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
    if candidate.starts_with(&workspace) || grant_allows(&candidate, grants) {
        return Ok(ResolvedPath(candidate));
    }
    Err(format!("path escapes workspace: {} (workspace: {})", candidate.display(), workspace.display()))
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

fn grant_allows(candidate: &Path, grants: &HashSet<PathBuf>) -> bool {
    grants.iter().any(|grant| {
        let Ok(grant) = canonicalize_lenient(grant) else {
            return false;
        };
        candidate == grant || (grant.is_dir() && candidate.starts_with(&grant))
    })
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

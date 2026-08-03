//! 跨 scope durable move。事务 claim 只存 app private data，项目内容永远不能提供
//! 可执行恢复权限；每次恢复都按 workspace/scope/slug 和精确派生路径重新验证。

mod claim;
mod path;
#[cfg(test)]
mod tests;
mod transfer;

use super::find_entry_with_home;
use crate::knowledge::Scope;
use claim::{MoveClaim, begin_claim, claim_path, locate_claim, validate_claim};
use path::{
    canonical_directory, canonical_scope_root, ensure_safe_parent, path_present, prepare_private_claim_root, reject_symlink_tree,
    staging_path, validate_relative,
};
use std::path::{Path, PathBuf};
use transfer::execute_claim;

pub fn move_entry(scope: Scope, workdir: &Path, slug: &str, to: Scope) -> Result<String, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    let claim_root = crate::core::paths::data_dir().join("knowledge-moves");
    move_entry_with_roots(scope, workdir, &home, slug, to, &claim_root)
}

#[cfg(test)]
pub(super) fn move_entry_with_home(scope: Scope, workdir: &Path, home: &Path, slug: &str, to: Scope) -> Result<String, String> {
    std::fs::create_dir_all(home).map_err(|error| format!("create test home {}: {error}", home.display()))?;
    move_entry_with_roots(scope, workdir, home, slug, to, &home.join(".kxen-private/knowledge-moves"))
}

fn move_entry_with_roots(scope: Scope, workdir: &Path, home: &Path, slug: &str, to: Scope, claim_root: &Path) -> Result<String, String> {
    if scope == to {
        return Err("scope 相同".into());
    }
    if slug.trim().is_empty() {
        return Err("knowledge move slug cannot be empty".into());
    }
    let _move_guard = move_lock().lock().map_err(|error| error.to_string())?;
    let workspace = canonical_directory(workdir, "workspace")?;
    let home = canonical_directory(home, "home")?;
    let claim_root = prepare_private_claim_root(claim_root)?;

    if let Some((claim_path, claim)) = locate_claim(&claim_root, &workspace, scope, to, slug)? {
        validate_claim(&claim, &workspace, &home, workdir, scope, to, slug)?;
        return execute_claim(&claim_path, &claim, false);
    }

    let entry = find_entry_with_home(scope, workdir, &home, slug)?;
    if entry.is_agents_md {
        return Err("root interoperability rule cannot move between scopes".into());
    }
    let source_root = canonical_scope_root(scope, &workspace, &home, false)?;
    let destination_root = canonical_scope_root(to, &workspace, &home, true)?;
    let source_input = if entry.dir.is_empty() { PathBuf::from(&entry.path) } else { PathBuf::from(&entry.dir) };
    reject_symlink_tree(&source_input)?;
    let source =
        source_input.canonicalize().map_err(|error| format!("canonicalize knowledge source {}: {error}", source_input.display()))?;
    let relative =
        source.strip_prefix(&source_root).map_err(|_| format!("source is outside scope root: {}", source.display()))?.to_path_buf();
    validate_relative(&relative)?;
    let destination = destination_root.join(&relative);
    if path_present(&destination)? {
        return Err(format!("destination already exists: {}", destination.display()));
    }
    ensure_safe_parent(&destination_root, destination.parent().ok_or("destination has no parent")?, true)?;
    let transaction_id = crate::core::ids::new_id("move");
    let staging = staging_path(&destination, &transaction_id)?;
    if path_present(&staging)? {
        return Err(format!("knowledge move staging already exists: {}", staging.display()));
    }
    let claim = MoveClaim {
        version: claim::CLAIM_VERSION,
        transaction_id,
        workspace: workspace.clone(),
        scope,
        to,
        requested_slug: slug.to_string(),
        entry_slug: entry.slug,
        source_root,
        destination_root,
        relative,
        source,
        destination,
        staging,
    };
    let claim_path = claim_path(&claim_root, &claim.workspace, scope, to, &claim.entry_slug);
    begin_claim(&claim_path, &claim)?;
    validate_claim(&claim, &workspace, &home, workdir, scope, to, slug)?;
    execute_claim(&claim_path, &claim, false)
}

fn move_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

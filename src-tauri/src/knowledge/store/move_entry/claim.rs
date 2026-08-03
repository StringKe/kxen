use super::super::find_entry_with_home;
use super::path::{
    canonical_scope_root, ensure_safe_parent, path_present, reject_symlink_tree, staging_path, sync_move_directory, validate_relative,
};
use crate::knowledge::Scope;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(super) const CLAIM_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MoveClaim {
    pub(super) version: u32,
    pub(super) transaction_id: String,
    pub(super) workspace: PathBuf,
    pub(super) scope: Scope,
    pub(super) to: Scope,
    pub(super) requested_slug: String,
    pub(super) entry_slug: String,
    pub(super) source_root: PathBuf,
    pub(super) destination_root: PathBuf,
    pub(super) relative: PathBuf,
    pub(super) source: PathBuf,
    pub(super) destination: PathBuf,
    pub(super) staging: PathBuf,
}

pub(super) fn claim_path(root: &Path, workspace: &Path, scope: Scope, to: Scope, entry_slug: &str) -> PathBuf {
    let key = format!("{}\0{}\0{}\0{entry_slug}", workspace.display(), scope.as_str(), to.as_str());
    let digest = sha2::Sha256::digest(key.as_bytes());
    root.join(format!("{:x}.json", digest))
}

pub(super) fn locate_claim(
    root: &Path,
    workspace: &Path,
    scope: Scope,
    to: Scope,
    requested_slug: &str,
) -> Result<Option<(PathBuf, MoveClaim)>, String> {
    let direct = claim_path(root, workspace, scope, to, requested_slug);
    if path_present(&direct)? {
        return load_claim(&direct).map(|claim| Some((direct, claim)));
    }
    // 手输 description 首次会解析成 canonical entry slug。若 source 已在崩溃前消失，
    // retry 仍可按 claim 内 requested_slug 找回；目录是 private data，不接受项目提供的候选。
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| format!("scan knowledge move claims {}: {error}", root.display()))? {
        let entry = entry.map_err(|error| format!("read knowledge move claim entry: {error}"))?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|error| format!("inspect knowledge move claim {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let claim = match load_claim(&path) {
            Ok(claim) => claim,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "unrelated knowledge move claim is unreadable");
                continue;
            }
        };
        if claim.workspace == workspace
            && claim.scope == scope
            && claim.to == to
            && (claim.entry_slug == requested_slug || claim.requested_slug == requested_slug)
        {
            matches.push((path, claim));
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(format!("knowledge move is BLOCKED: multiple private claims match {requested_slug}")),
    }
}

pub(super) fn validate_claim(
    claim: &MoveClaim,
    workspace: &Path,
    home: &Path,
    workdir: &Path,
    scope: Scope,
    to: Scope,
    requested_slug: &str,
) -> Result<(), String> {
    if claim.version != CLAIM_VERSION {
        return Err(format!("unsupported knowledge move claim version {}", claim.version));
    }
    crate::core::ids::validate_id(&claim.transaction_id)?;
    if claim.workspace != workspace || claim.scope != scope || claim.to != to {
        return Err("knowledge move claim identity does not match the requested workspace/scopes".into());
    }
    if claim.entry_slug != requested_slug && claim.requested_slug != requested_slug {
        return Err("knowledge move claim does not match the requested slug".into());
    }
    let source_root = canonical_scope_root(scope, workspace, home, false)?;
    let destination_root = canonical_scope_root(to, workspace, home, true)?;
    validate_relative(&claim.relative)?;
    let expected_source = source_root.join(&claim.relative);
    let expected_destination = destination_root.join(&claim.relative);
    let expected_staging = staging_path(&expected_destination, &claim.transaction_id)?;
    if claim.source_root != source_root
        || claim.destination_root != destination_root
        || claim.source != expected_source
        || claim.destination != expected_destination
        || claim.staging != expected_staging
    {
        return Err("knowledge move claim paths do not match their exact derived identity".into());
    }
    ensure_safe_parent(&source_root, claim.source.parent().ok_or("source has no parent")?, false)?;
    ensure_safe_parent(&destination_root, claim.destination.parent().ok_or("destination has no parent")?, true)?;

    let source_present = path_present(&claim.source)?;
    let destination_present = path_present(&claim.destination)?;
    if source_present {
        reject_symlink_tree(&claim.source)?;
        let entry = find_entry_with_home(scope, workdir, home, &claim.entry_slug)?;
        let actual = if entry.dir.is_empty() { PathBuf::from(entry.path) } else { PathBuf::from(entry.dir) };
        let actual = actual.canonicalize().map_err(|error| format!("canonicalize requested knowledge entry: {error}"))?;
        if entry.slug != claim.entry_slug || actual != claim.source {
            return Err("knowledge move claim source is not the requested slug entry".into());
        }
    } else if destination_present {
        reject_symlink_tree(&claim.destination)?;
        let entry = find_entry_with_home(to, workdir, home, &claim.entry_slug)?;
        let actual = if entry.dir.is_empty() { PathBuf::from(entry.path) } else { PathBuf::from(entry.dir) };
        let actual = actual.canonicalize().map_err(|error| format!("canonicalize moved knowledge entry: {error}"))?;
        if entry.slug != claim.entry_slug || actual != claim.destination {
            return Err("knowledge move claim destination is not the requested slug entry".into());
        }
    }
    Ok(())
}

/// 初始 claim 使用 create_new，跨进程并发 move 只有一个 owner 能在 Provider-independent
/// 数据变更前取得 durable 权限。部分写入保留为 fail-closed recovery evidence。
pub(super) fn begin_claim(path: &Path, claim: &MoveClaim) -> Result<(), String> {
    let parent = path.parent().ok_or("move claim has no parent")?;
    let bytes = serde_json::to_vec_pretty(claim).map_err(|error| format!("serialize knowledge move claim: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| format!("claim knowledge move {}: {error}", path.display()))?;
    file.write_all(&bytes).map_err(|error| format!("write knowledge move claim {}: {error}", path.display()))?;
    file.sync_all().map_err(|error| format!("sync knowledge move claim {}: {error}", path.display()))?;
    sync_move_directory(parent)
}

/// 恢复 claim removal 的 visible-but-not-durable failure；允许原子替换同一事务记录。
pub(super) fn persist_claim(path: &Path, claim: &MoveClaim) -> Result<(), String> {
    let parent = path.parent().ok_or("move claim has no parent")?;
    let bytes = serde_json::to_vec_pretty(claim).map_err(|error| format!("serialize knowledge move claim: {error}"))?;
    let file_name = path.file_name().and_then(|name| name.to_str()).unwrap_or("claim.json");
    let tmp = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp).map_err(|error| format!("open {}: {error}", tmp.display()))?;
        file.write_all(&bytes).and_then(|()| file.sync_all()).map_err(|error| format!("write {}: {error}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|error| format!("publish {}: {error}", path.display()))?;
        sync_move_directory(parent)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

fn load_claim(path: &Path) -> Result<MoveClaim, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("knowledge move claim is not a regular file: {}", path.display()));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

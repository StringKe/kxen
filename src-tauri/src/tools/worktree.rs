//! worktree 隔离：git worktree 并行安全（批量迁移 / 并行修改）。
//! worktree 放 `<repo>/.kxen/worktrees/<name>`（自动把 .kxen/ 写进 .gitignore），分支 `kxen/<name>`。

use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
}

/// macOS 临时目录 /var 是 /private/var 的软链：git 输出全是不等价的真实路径，统一 canonicalize。
fn canon(repo: &Path) -> PathBuf {
    repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf())
}

/// worktree 名白名单校验（与 file-backed id 同规则：杜绝路径穿越进 .kxen/worktrees/<name>）。
pub fn validate_name(name: &str) -> Result<(), String> {
    if crate::core::ids::is_valid_id(name) { Ok(()) } else { Err(format!("invalid worktree name: {name:?}")) }
}

/// 创建 worktree（已存在则直接复用）。
pub async fn create(repo: &Path, name: &str) -> Result<WorktreeInfo, String> {
    let repo = &canon(repo);
    validate_name(name)?;
    ensure_gitignore(repo)?;
    let path = repo.join(".kxen").join("worktrees").join(name);
    let branch = format!("kxen/{name}");
    if path.exists() {
        return Ok(WorktreeInfo { name: name.into(), path, branch });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    git(repo, &["worktree", "add", &path.to_string_lossy(), "-b", &branch]).await?;
    Ok(WorktreeInfo { name: name.into(), path, branch })
}

/// 移除 worktree（分支默认保留，用户自行 merge/diff 后处理）。
/// 无审批通道的旧入口：dirty 或 delete_branch 一律拒绝，clean 且保留分支才放行。
pub async fn remove(repo: &Path, name: &str, delete_branch: bool) -> Result<(), String> {
    remove_with_approval(repo, name, delete_branch, None).await
}

/// 移除 worktree（审批门变体）：dirty（有改动可丢）或 delete_branch（删分支不可逆）时，
/// 有审批通道则挂起等用户决定；无通道拒绝。
pub async fn remove_with_approval(
    repo: &Path,
    name: &str,
    delete_branch: bool,
    approval: Option<&crate::tools::exec::ApprovalCtx<'_>>,
) -> Result<(), String> {
    let repo = &canon(repo);
    validate_name(name)?;
    let path = repo.join(".kxen").join("worktrees").join(name);

    // dirty 判定：worktree 内的 git status（未跟踪文件也算有改动可丢）；计数进审批理由（用户要知道丢几个文件）
    let dirty_count =
        if path.exists() { git(&path, &["status", "--porcelain"]).await?.lines().filter(|l| !l.trim().is_empty()).count() } else { 0 };
    let dirty = dirty_count > 0;

    if dirty || delete_branch {
        let mut command = format!("git worktree remove {name}");
        let mut reasons: Vec<String> = Vec::new();
        if dirty {
            reasons.push(format!("worktree {name} 有 {dirty_count} 个文件未提交改动，删除将丢失"));
        }
        if delete_branch {
            command.push_str(&format!(" && git branch -D kxen/{name}"));
            reasons.push(format!("删除分支 kxen/{name}（不可恢复）"));
        }
        let reason = reasons.join("；");
        let Some(appr) = approval else {
            return Err(format!("{reason}（当前上下文无审批通道，按拒绝处理）"));
        };
        match crate::agent::approval::request_approval(appr, &command, &reason).await {
            crate::agent::approval::ApprovalOutcome::Allow => {}
            crate::agent::approval::ApprovalOutcome::Timeout => {
                return Err(format!("{reason}（用户超时未响应）"));
            }
            crate::agent::approval::ApprovalOutcome::Deny => {
                return Err(format!("{reason}（用户拒绝或已中断）"));
            }
        }
    }

    if path.exists() {
        git(repo, &["worktree", "remove", "--force", &path.to_string_lossy()]).await?;
    }
    if delete_branch {
        git(repo, &["branch", "-D", &format!("kxen/{name}")]).await?;
    }
    Ok(())
}

pub async fn list(repo: &Path) -> Result<Vec<WorktreeInfo>, String> {
    let repo = &canon(repo);
    let out = git(repo, &["worktree", "list", "--porcelain"]).await?;
    let mut infos = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = String::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(p) = path.take() {
                infos.push((p, std::mem::take(&mut branch)));
            }
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = b.to_string();
        }
    }
    if let Some(p) = path {
        infos.push((p, branch));
    }
    // 前缀过滤按主仓库根算：切进隔离树后入参是 worktree 路径，直接 join 会让过滤全部落空（看板变空）
    let prefix = main_repo_root(repo).await?.join(".kxen").join("worktrees");
    Ok(infos
        .into_iter()
        .filter_map(|(p, branch)| {
            let name = p.strip_prefix(&prefix).ok()?.to_string_lossy().into_owned();
            Some(WorktreeInfo { name, path: p, branch })
        })
        .collect())
}

/// 主仓库根解析：worktree 内执行 git 时 --git-common-dir 指向主树的 .git，取其父目录。
/// 非常规布局（bare / 自定义 gitdir）取不到 .git 尾段，回退入参。
async fn main_repo_root(repo: &Path) -> Result<PathBuf, String> {
    let out = git(repo, &["rev-parse", "--git-common-dir"]).await?;
    let raw = out.trim();
    let git_dir = canon(&if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { repo.join(raw) });
    Ok(match git_dir.file_name() {
        Some(name) if name == ".git" => git_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| repo.to_path_buf()),
        _ => repo.to_path_buf(),
    })
}

/// 当前树相对 worktree 分支的 diff --stat（完成回主树的预览）。
pub async fn diff_stat(repo: &Path, name: &str) -> Result<String, String> {
    validate_name(name)?;
    git(repo, &["diff", "--stat", &format!("kxen/{name}")]).await
}

/// worktree 工具路由（agent_loop 执行层拆出）：create/remove/list/diff 四动作。
pub async fn tool_dispatch(repo: &Path, args: &Value, approval: Option<&crate::tools::exec::ApprovalCtx<'_>>) -> Result<String, String> {
    match args.get("action").and_then(Value::as_str).ok_or("missing action")? {
        "create" => {
            let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let info = create(repo, name).await?;
            Ok(format!("worktree {} at {} (branch {})", info.name, info.path.display(), info.branch))
        }
        "remove" => {
            let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let delete_branch = args.get("delete_branch").and_then(Value::as_bool).unwrap_or(false);
            remove_with_approval(repo, name, delete_branch, approval).await?;
            Ok(format!("removed worktree {name}{}", if delete_branch { " (branch deleted)" } else { " (branch kept)" }))
        }
        "list" => {
            let list = list(repo).await?;
            Ok(if list.is_empty() {
                "no kxen worktrees".into()
            } else {
                list.iter().map(|i| format!("{} -> {} ({})", i.name, i.path.display(), i.branch)).collect::<Vec<_>>().join("\n")
            })
        }
        "diff" => {
            let name = args.get("name").and_then(Value::as_str).ok_or("missing name")?;
            let stat = diff_stat(repo, name).await?;
            Ok(if stat.trim().is_empty() { "no changes on branch".into() } else { stat })
        }
        other => Err(format!("unknown worktree action: {other}")),
    }
}

// ---------------- 通用 git 状态/diff（dock 改动面板数据源） ----------------

#[derive(Debug, serde::Serialize)]
pub struct StatusEntry {
    pub path: String,
    /// M / A / D / ??（取 porcelain 首列，重命名取 R）
    pub status: String,
}

/// git status --porcelain（未暂存 + 未跟踪，dock 的改动清单）。
pub async fn status(repo: &Path) -> Result<Vec<StatusEntry>, String> {
    let repo = &canon(repo);
    let out = git(repo, &["status", "--porcelain"]).await?;
    Ok(out
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| {
            let code = l[..2].trim().to_string();
            // 重命名 "R  old -> new" 取新路径
            let path = l[3..].rsplit(" -> ").next().unwrap_or(&l[3..]).to_string();
            StatusEntry { path, status: code }
        })
        .collect())
}

/// 单文件 diff（未暂存）；未跟踪文件走 --no-index 合成 new-file diff。
pub async fn diff_file(repo: &Path, path: &str) -> Result<String, String> {
    let repo = &canon(repo);
    let diff = git(repo, &["diff", "--", path]).await.unwrap_or_default();
    if !diff.trim().is_empty() {
        return Ok(diff);
    }
    // --no-index 命中差异时退出码为 1：走容忍路径
    let out = tokio::process::Command::new("git")
        .args(["diff", "--no-index", "--", "/dev/null", path])
        .current_dir(repo)
        .output()
        .await
        .map_err(|e| format!("git spawn: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() { Err("no diff (unchanged or not a file)".into()) } else { Ok(text) }
}

/// .kxen/ 进 .gitignore（幂等）。fs_tool 的覆盖备份也落在 .kxen/ 下，共用此入口。
pub(crate) fn ensure_gitignore(repo: &Path) -> Result<(), String> {
    let path = repo.join(".gitignore");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if content.lines().any(|l| l.trim() == ".kxen/") {
        return Ok(());
    }
    let mut new = content;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(".kxen/\n");
    std::fs::write(&path, new).map_err(|e| e.to_string())
}

async fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new("git").args(args).current_dir(repo).output().await.map_err(|e| format!("git spawn: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).chars().take(300).collect::<String>()
        ))
    }
}

//! checkpoint/rewind：shadow git（bare repo 存全局数据目录，--git-dir + --work-tree 不污染项目）。
//! 每用户消息一个检查点（turn 前状态）；rewind = reset 到该消息 commit + 会话截断。
//! 排除 node_modules/target（体量大且可再生）。

use std::path::{Path, PathBuf};

const EXCLUDES: &[&str] = &[":(exclude)node_modules", ":(exclude)target", ":(exclude).kxen/worktrees"];

fn repo_dir(workdir: &Path) -> PathBuf {
    use sha2::Digest;
    // canonicalize：/var 与 /private/var 这类拼写分叉必须收敛到同一 shadow repo；
    // sha256 取代 DefaultHasher：后者输出跨 Rust 版本不受保证，工具链升级会让存量检查点变孤儿。
    let path = workdir.canonicalize().unwrap_or_else(|_| workdir.to_path_buf());
    let digest = sha2::Sha256::digest(path.to_string_lossy().as_bytes());
    crate::core::paths::data_dir().join("shadow").join(format!("{:x}.git", digest))
}

fn git(workdir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("git")
        .arg(format!("--git-dir={}", repo_dir(workdir).display()))
        .arg(format!("--work-tree={}", workdir.display()))
        .args(args)
        .output()
        .map_err(|e| format!("git spawn: {e}"))
}

fn ensure_repo(workdir: &Path) -> Result<(), String> {
    let dir = repo_dir(workdir);
    // shadow repo 存工作区全量快照：0700 仅属主可进（与 auth.json 0600 同一加固口径）。
    // 每次调用都设：存量默认 umask 创建的 repo 一并收紧。
    if dir.join("HEAD").exists() {
        harden_dir(&dir)?;
        return Ok(());
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    harden_dir(&dir)?;
    // init 不带 --work-tree（init 不接受该参数）；后续操作才走 --git-dir/--work-tree
    let out = std::process::Command::new("git").args(["init", "--bare"]).arg(&dir).output().map_err(|e| format!("git spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("git init: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

#[cfg(unix)]
fn harden_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn harden_dir(_dir: &Path) -> Result<(), String> {
    Ok(())
}

/// 同 workspace 多会话并发 checkpoint 会撞 index.lock：按 shadow repo 加进程内互斥。
fn repo_lock(workdir: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    static LOCKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<()>>>>> =
        std::sync::LazyLock::new(Default::default);
    crate::core::shared::lock(&LOCKS).entry(repo_dir(workdir)).or_default().clone()
}

/// commit 固定 -c 配置：user 身份 + 强制关签名。
/// shadow repo 无需签名；全局开着 gpgsign（如本机 1Password op-ssh-sign）时测试环境没有可用签名程序，commit 会直接失败
fn commit_args(label: &str) -> [&str; 13] {
    [
        "-c",
        "user.name=kxen",
        "-c",
        "user.email=kxen@app",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--allow-empty",
        "--allow-empty-message",
        "--no-verify",
        "-q",
        "-m",
        label,
    ]
}

/// 打检查点：当前 worktree 全量提交（无变更也成功返回）。
pub fn commit(workdir: &Path, label: &str) -> Result<(), String> {
    // 互斥覆盖 init + add + commit 全段：并发 ensure_repo 会双双 git init 撞模板拷贝，
    // 并发 add/commit 撞 index.lock。find/dirty_count 只读不锁
    let lock = repo_lock(workdir);
    let _guard = crate::core::shared::lock(&lock);
    commit_locked(workdir, label)
}

fn commit_locked(workdir: &Path, label: &str) -> Result<(), String> {
    ensure_repo(workdir)?;
    if has_head(workdir)? && find(workdir, label)?.is_some() {
        return Ok(());
    }
    let mut add_args = vec!["add", "-A", "--", "."];
    add_args.extend(EXCLUDES);
    let out = git(workdir, &add_args)?;
    if !out.status.success() {
        return Err(format!("git add: {}", String::from_utf8_lossy(&out.stderr)));
    }
    // 每个 user message id 都是 rewind label。tree 未变化也必须生成 allow-empty commit；
    // 同 label 的 queue/restart 重放由上面的 find 幂等收敛，不重复制造 checkpoint。
    let out = git(workdir, &commit_args(label))?;
    if !out.status.success() {
        return Err(format!("git commit: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

fn has_head(workdir: &Path) -> Result<bool, String> {
    let out = git(workdir, &["rev-parse", "--verify", "HEAD"])?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(128) => Ok(false),
        _ => Err(format!("git rev-parse HEAD: {}", String::from_utf8_lossy(&out.stderr))),
    }
}

/// 按 label 找 commit hash。
fn find(workdir: &Path, label: &str) -> Result<Option<String>, String> {
    let out = git(workdir, &["log", "--format=%H%x00%B%x00", "-z"])?;
    if !out.status.success() {
        return Err(format!("git log: {}", String::from_utf8_lossy(&out.stderr)));
    }
    // %x00 与 -z 各发一个 NUL：记录间是双 NUL，先滤空再两两成对
    let text = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
    for rec in parts.chunks(2) {
        if rec.len() == 2 && rec[1].trim() == label {
            return Ok(Some(rec[0].trim().to_string()));
        }
    }
    Ok(None)
}

fn head(workdir: &Path) -> Result<String, String> {
    let out = git(workdir, &["rev-parse", "HEAD"])?;
    if !out.status.success() {
        return Err(format!("git rev-parse: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn reset_hard(workdir: &Path, hash: &str) -> Result<(), String> {
    let out = git(workdir, &["reset", "--hard", hash])?;
    if out.status.success() { Ok(()) } else { Err(format!("git reset: {}", String::from_utf8_lossy(&out.stderr))) }
}

fn clean(workdir: &Path) -> Result<(), String> {
    let mut args = vec!["clean", "-fdq", "--", "."];
    args.extend(EXCLUDES);
    let out = git(workdir, &args)?;
    if out.status.success() { Ok(()) } else { Err(format!("git clean: {}", String::from_utf8_lossy(&out.stderr))) }
}

struct EmptyDir {
    path: PathBuf,
    permissions: std::fs::Permissions,
}

fn empty_dirs(workdir: &Path) -> Result<Vec<EmptyDir>, String> {
    fn walk(root: &Path, current: &Path, output: &mut Vec<EmptyDir>) -> Result<(), String> {
        let entries: Vec<_> = std::fs::read_dir(current)
            .map_err(|error| format!("read workspace directory {}: {error}", current.display()))?
            .collect::<Result<_, _>>()
            .map_err(|error| error.to_string())?;
        if current != root {
            let metadata = std::fs::metadata(current).map_err(|error| error.to_string())?;
            output
                .push(EmptyDir { path: current.strip_prefix(root).unwrap_or(current).to_path_buf(), permissions: metadata.permissions() });
        }
        if entries.is_empty() {
            return Ok(());
        }
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            if !path.is_dir() || [".git", "node_modules", "target"].iter().any(|skip| name == std::ffi::OsStr::new(skip)) {
                continue;
            }
            if path.strip_prefix(root).is_ok_and(|relative| relative == Path::new(".kxen/worktrees")) {
                continue;
            }
            walk(root, &path, output)?;
        }
        Ok(())
    }
    let mut output = Vec::new();
    walk(workdir, workdir, &mut output)?;
    Ok(output)
}

fn restore_empty_dirs(workdir: &Path, directories: &[EmptyDir]) -> Result<(), String> {
    for directory in directories {
        let path = workdir.join(&directory.path);
        std::fs::create_dir_all(&path).map_err(|error| format!("restore empty directory {}: {error}", path.display()))?;
        std::fs::set_permissions(&path, directory.permissions.clone())
            .map_err(|error| format!("restore permissions {}: {error}", path.display()))?;
    }
    Ok(())
}

fn rollback(workdir: &Path, backup: &str, directories: &[EmptyDir], cause: String) -> String {
    match reset_hard(workdir, backup).and_then(|()| clean(workdir)).and_then(|()| restore_empty_dirs(workdir, directories)) {
        Ok(()) => cause,
        Err(error) => format!("{cause}; workspace rollback failed: {error}"),
    }
}

/// 原子 rewind：先把当前 workspace 状态提交为内部补偿点，再执行 reset + clean + 调用方持久化。
/// 任一步失败都回到补偿点，避免 workspace 已回退而 transcript 仍停在未来状态。
pub fn rewind<T>(workdir: &Path, label: &str, persist: impl FnOnce() -> Result<T, String>) -> Result<(String, T), String> {
    rewind_with_clean(workdir, label, persist, clean)
}

fn rewind_with_clean<T>(
    workdir: &Path,
    label: &str,
    persist: impl FnOnce() -> Result<T, String>,
    clean_step: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(String, T), String> {
    // 与 commit 同一把锁：补偿点、reset/clean 与 transcript 提交期间不能有并发 add 改写 index。
    let lock = repo_lock(workdir);
    let _guard = crate::core::shared::lock(&lock);
    let Some(target) = find(workdir, label)? else {
        return Err(format!("checkpoint not found: {label}"));
    };
    let backup_label = format!("kxen-rewind-backup-{}-{}", std::process::id(), crate::core::shared::now_ms());
    commit_locked(workdir, &backup_label)?;
    let backup = head(workdir)?;
    let directories = empty_dirs(workdir)?;

    if let Err(error) = reset_hard(workdir, &target) {
        return Err(rollback(workdir, &backup, &directories, error));
    }
    if let Err(error) = clean_step(workdir) {
        return Err(rollback(workdir, &backup, &directories, error));
    }
    match persist() {
        Ok(value) => Ok((target, value)),
        Err(error) => Err(rollback(workdir, &backup, &directories, error)),
    }
}

/// 仅回退 workspace 的兼容入口。
pub fn reset_to(workdir: &Path, label: &str) -> Result<String, String> {
    rewind(workdir, label, || Ok(())).map(|(hash, ())| hash)
}

/// 会话是否有 rewind 历史可导（首条 checkpoint 是否存在）。
pub fn has_checkpoints(workdir: &Path) -> bool {
    repo_dir(workdir).join("HEAD").exists()
}

/// shadow 仓库未进检查点的改动文件数（rewind 确认框展示「会丢弃几个文件」的数据源）。
/// 与 commit 同一组排除（node_modules/target）：否则可再生目录会让判定永远为脏。
pub fn dirty_count(workdir: &Path) -> Result<usize, String> {
    if !has_checkpoints(workdir) {
        return Ok(0);
    }
    let lock = repo_lock(workdir);
    let _guard = crate::core::shared::lock(&lock);
    let mut args = vec!["status", "--porcelain", "--", "."];
    args.extend(EXCLUDES);
    let out = git(workdir, &args)?;
    if !out.status.success() {
        return Err(format!("git status: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().filter(|line| !line.trim().is_empty()).count())
}

/// checkpoint 屏障：用户消息落盘后、run_turn 前等 shadow git commit 完成。
/// 失败必须阻止 workspace mutation；否则 transcript 已接受消息却没有对应 rewind 真源。
pub async fn checkpoint_barrier(workdir: &Path, label: &str) -> Result<(), String> {
    let dir = workdir.to_path_buf();
    let label = label.to_string();
    match tokio::task::spawn_blocking(move || commit(&dir, &label)).await {
        Ok(result) => result,
        Err(error) => Err(format!("checkpoint commit join failed: {error}")),
    }
}

#[cfg(test)]
mod tests;

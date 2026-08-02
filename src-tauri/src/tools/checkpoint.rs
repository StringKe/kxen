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
fn commit_args(label: &str) -> [&str; 12] {
    [
        "-c",
        "user.name=kxen",
        "-c",
        "user.email=kxen@app",
        "-c",
        "commit.gpgsign=false",
        "commit",
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
    ensure_repo(workdir)?;
    let mut add_args = vec!["add", "-A", "--", "."];
    add_args.extend(EXCLUDES);
    let out = git(workdir, &add_args)?;
    if !out.status.success() {
        return Err(format!("git add: {}", String::from_utf8_lossy(&out.stderr)));
    }
    // 无变更时跳过 commit：用 diff --cached 预判，而不是匹配 "nothing to commit" 文案
    // （git 本地化输出下该文案在 stdout 且非英文，匹配不可靠）
    let staged = git(workdir, &["diff", "--cached", "--quiet", "--exit-code"])?;
    if staged.status.code() == Some(0) {
        return Ok(());
    }
    let out = git(workdir, &commit_args(label))?;
    if !out.status.success() {
        return Err(format!("git commit: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(())
}

/// 按 label 找 commit hash。
fn find(workdir: &Path, label: &str) -> Result<Option<String>, String> {
    let out = git(workdir, &["log", "--format=%H%x00%B%x00", "-z"])?;
    if !out.status.success() {
        return Ok(None);
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

/// rewind 到 label 检查点（reset --hard + clean 未跟踪，调用方自行负责会话截断与提示）。
pub fn reset_to(workdir: &Path, label: &str) -> Result<String, String> {
    let Some(hash) = find(workdir, label)? else {
        return Err(format!("checkpoint not found: {label}"));
    };
    // 与 commit 同一把锁：reset/clean 进行中不能有并发 add 改写 index
    let lock = repo_lock(workdir);
    let _guard = crate::core::shared::lock(&lock);
    let out = git(workdir, &["reset", "--hard", &hash])?;
    if !out.status.success() {
        return Err(format!("git reset: {}", String::from_utf8_lossy(&out.stderr)));
    }
    // commit 的 add -A 已把检查点时刻全部未跟踪文件入库，故 reset 后仍是未跟踪的一定是
    // 检查点之后新建的文件——确认框承诺「回退将丢弃」必须连它们一起清（gitignored 文件不动）。
    let mut clean_args = vec!["clean", "-fdq", "--", "."];
    clean_args.extend(EXCLUDES);
    let out = git(workdir, &clean_args)?;
    if out.status.success() { Ok(hash) } else { Err(format!("git clean: {}", String::from_utf8_lossy(&out.stderr))) }
}

/// 会话是否有 rewind 历史可导（首条 checkpoint 是否存在）。
pub fn has_checkpoints(workdir: &Path) -> bool {
    repo_dir(workdir).join("HEAD").exists()
}

/// shadow 仓库未进检查点的改动文件数（rewind 确认框展示「会丢弃几个文件」的数据源）。
/// 与 commit 同一组排除（node_modules/target）：否则可再生目录会让判定永远为脏。
pub fn dirty_count(workdir: &Path) -> usize {
    if !has_checkpoints(workdir) {
        return 0;
    }
    let mut args = vec!["status", "--porcelain", "--", "."];
    args.extend(EXCLUDES);
    // porcelain 一文件一行，空仓库输出空串
    git(workdir, &args).map(|out| String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0)
}

/// checkpoint 屏障：用户消息落盘后、run_turn 前等 shadow git commit 完成。
/// 失败只 warn 不阻塞（checkpoint 是可再生优化，不能卡死主流程）。
pub async fn checkpoint_barrier(workdir: &Path, label: &str) {
    let dir = workdir.to_path_buf();
    let label = label.to_string();
    match tokio::task::spawn_blocking(move || commit(&dir, &label)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "checkpoint commit failed"),
        Err(e) => tracing::warn!(error = %e, "checkpoint commit join failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_dir_stable_and_canonical() {
        use sha2::Digest;
        let base = std::env::temp_dir().join(format!("kxen-ckpt-hash-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        // 同一路径多次调用哈希稳定，格式 = sha256 64 hex + .git
        let dir = repo_dir(&real);
        assert_eq!(dir, repo_dir(&real));
        let expect = format!("{:x}", sha2::Sha256::digest(real.canonicalize().unwrap().to_string_lossy().as_bytes()));
        assert!(dir.ends_with(format!("{expect}.git")), "寻址必须是 canonical 路径的 sha256: {}", dir.display());
        // symlink 拼写分叉（link 与 real 指向同一目录）必须收敛到同一 shadow repo
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(dir, repo_dir(&link));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn commit_args_disable_gpgsign() {
        let args = commit_args("x");
        assert!(
            args.windows(2).any(|w| w == ["-c", "commit.gpgsign=false"]),
            "shadow commit 必须显式关 gpgsign（全局 1Password 签名会让 commit 失败）"
        );
    }

    #[test]
    fn commit_ignores_repo_level_gpgsign() {
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        // repo 级强制签名 + 必然失败的签名程序：少了 -c commit.gpgsign=false 则这次 commit 必败
        git(&dir, &["config", "commit.gpgsign", "true"]).unwrap();
        git(&dir, &["config", "gpg.program", "/bin/false"]).unwrap();
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        commit(&dir, "msg_2").unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dirty_count_tracks_uncheckpointed_files() {
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-dirty-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        assert_eq!(dirty_count(&dir), 0);
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        assert_eq!(dirty_count(&dir), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_and_rewind() {
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        commit(&dir, "msg_2").unwrap();
        // rewind 到 msg_1：a.txt 回到 v1
        reset_to(&dir, "msg_1").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n");
        // 不存在的 label 报错
        assert!(reset_to(&dir, "msg_404").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rewind_removes_files_created_after_checkpoint() {
        // 与确认框承诺一致：检查点之后新建的文件（turn 内 agent 产物）在 rewind 后不复存在
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-clean-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        // turn 内：新建文件、新建子目录文件、改动既有文件
        std::fs::write(dir.join("new.txt"), "agent new\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/nested.txt"), "agent nested\n").unwrap();
        std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
        reset_to(&dir, "msg_1").unwrap();
        assert!(!dir.join("new.txt").exists(), "检查点后新建文件必须被清除");
        assert!(!dir.join("sub").exists(), "检查点后新建目录必须被清除");
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rewind_keeps_preexisting_untracked_files() {
        // 检查点时刻已存在的未跟踪文件已被 add -A 入库：rewind 不得误删用户自己的文件
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-keep-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        commit(&dir, "msg_2").unwrap();
        reset_to(&dir, "msg_1").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n", "检查点前已存在的文件必须保留");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn concurrent_commits_do_not_collide_on_index_lock() {
        // 同 workspace 多会话并发 checkpoint：进程内互斥保证不撞 index.lock
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-conc-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        let mut handles = Vec::new();
        for i in 0..4 {
            let d = dir.clone();
            handles.push(std::thread::spawn(move || commit(&d, &format!("msg_{i}"))));
        }
        for h in handles {
            h.join().unwrap().unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn shadow_repo_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kxen-ckpt-perm-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
        commit(&dir, "msg_1").unwrap();
        let mode = std::fs::metadata(repo_dir(&dir)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "shadow repo 目录必须 0700: {mode:o}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

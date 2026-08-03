use super::*;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-ckpt-{tag}-{}-{}", std::process::id(), crate::core::shared::now_ms()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn repo_dir_stable_and_canonical() {
    use sha2::Digest;
    let base = fixture("hash");
    let real = base.join("real");
    std::fs::create_dir_all(&real).unwrap();
    let dir = repo_dir(&real);
    assert_eq!(dir, repo_dir(&real));
    let expect = format!("{:x}", sha2::Sha256::digest(real.canonicalize().unwrap().to_string_lossy().as_bytes()));
    assert!(dir.ends_with(format!("{expect}.git")), "寻址必须是 canonical 路径的 sha256: {}", dir.display());
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(dir, repo_dir(&link));
    std::fs::remove_dir_all(&base).ok();
}

#[test]
fn commit_args_disable_gpgsign() {
    let args = commit_args("x");
    assert!(args.windows(2).any(|w| w == ["-c", "commit.gpgsign=false"]));
    assert!(args.contains(&"--allow-empty"));
}

#[test]
fn unchanged_tree_gets_one_idempotent_checkpoint_per_message_label() {
    let dir = fixture("empty-label");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    let first = head(&dir).unwrap();

    commit(&dir, "msg_2").unwrap();
    let second = head(&dir).unwrap();
    assert_ne!(first, second, "unchanged tree still needs a distinct rewind label");
    assert_eq!(find(&dir, "msg_2").unwrap().as_deref(), Some(second.as_str()));

    commit(&dir, "msg_2").unwrap();
    assert_eq!(head(&dir).unwrap(), second, "replaying the same message id must not duplicate checkpoints");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn commit_ignores_repo_level_gpgsign() {
    let dir = fixture("sign");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    git(&dir, &["config", "commit.gpgsign", "true"]).unwrap();
    git(&dir, &["config", "gpg.program", "/bin/false"]).unwrap();
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    commit(&dir, "msg_2").unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dirty_count_tracks_uncheckpointed_files() {
    let dir = fixture("dirty");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    assert_eq!(dirty_count(&dir).unwrap(), 0);
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    std::fs::write(dir.join("b.txt"), "new\n").unwrap();
    assert_eq!(dirty_count(&dir).unwrap(), 2);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dirty_count_surfaces_git_failure() {
    let dir = fixture("dirty-error");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    std::fs::rename(repo_dir(&dir).join("objects"), repo_dir(&dir).join("objects.off")).unwrap();
    let error = dirty_count(&dir).expect_err("git status failure must not be reported as clean");
    assert!(error.contains("git status"), "unexpected error: {error}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn commit_and_rewind() {
    let dir = fixture("rewind");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    commit(&dir, "msg_2").unwrap();
    reset_to(&dir, "msg_1").unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n");
    assert!(reset_to(&dir, "msg_404").is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn failed_transcript_update_restores_exact_workspace() {
    let dir = fixture("transcript-rollback");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    commit(&dir, "msg_2").unwrap();
    std::fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();
    std::fs::create_dir_all(dir.join("empty/nested")).unwrap();

    let error = rewind(&dir, "msg_1", || Err::<(), _>("transcript failed".into())).unwrap_err();
    assert!(error.contains("transcript failed"));
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "dirty\n");
    assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "new\n");
    assert!(dir.join("empty/nested").is_dir(), "git 无法跟踪的空目录也必须被补偿恢复");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn failed_clean_restores_pre_rewind_workspace() {
    let dir = fixture("clean-rollback");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    commit(&dir, "msg_2").unwrap();

    let error = rewind_with_clean(&dir, "msg_1", || Ok::<(), String>(()), |_| Err("forced clean failure".into())).unwrap_err();
    assert!(error.contains("forced clean failure"));
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v2\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rewind_removes_files_created_after_checkpoint() {
    let dir = fixture("clean");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    std::fs::write(dir.join("new.txt"), "agent new\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/nested.txt"), "agent nested\n").unwrap();
    std::fs::write(dir.join("a.txt"), "v2\n").unwrap();
    reset_to(&dir, "msg_1").unwrap();
    assert!(!dir.join("new.txt").exists());
    assert!(!dir.join("sub").exists());
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rewind_keeps_preexisting_untracked_files() {
    let dir = fixture("keep");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    commit(&dir, "msg_2").unwrap();
    reset_to(&dir, "msg_1").unwrap();
    assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "v1\n");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn concurrent_commits_do_not_collide_on_index_lock() {
    let dir = fixture("conc");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    let mut handles = Vec::new();
    for i in 0..4 {
        let d = dir.clone();
        handles.push(std::thread::spawn(move || commit(&d, &format!("msg_{i}"))));
    }
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn shadow_repo_dir_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture("perm");
    std::fs::write(dir.join("a.txt"), "v1\n").unwrap();
    commit(&dir, "msg_1").unwrap();
    let mode = std::fs::metadata(repo_dir(&dir)).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    std::fs::remove_dir_all(&dir).ok();
}

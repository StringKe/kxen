use super::claim::persist_claim;
use super::*;

fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("kxen-move-{tag}-{}", uuid::Uuid::new_v4()));
    let workspace = root.join("workspace");
    let home = root.join("home");
    std::fs::create_dir_all(workspace.join(".agents/notes")).unwrap();
    std::fs::create_dir_all(home.join(".agents/notes")).unwrap();
    (root, workspace, home)
}

#[test]
fn copy_publish_recovers_after_destination_becomes_visible() {
    let (root, workspace, home) = fixture("recover");
    let source = workspace.join(".agents/notes/note.md");
    std::fs::write(&source, "---\ndescription: note\n---\ndurable\n").unwrap();
    let workspace_canonical = workspace.canonicalize().unwrap();
    let home_canonical = home.canonicalize().unwrap();
    let claim_root = prepare_private_claim_root(&home.join("private")).unwrap();
    let source_root = workspace.join(".agents").canonicalize().unwrap();
    let destination_root = home.join(".agents").canonicalize().unwrap();
    let transaction_id = "move_test_recover".to_string();
    let destination = destination_root.join("notes/note.md");
    let claim = MoveClaim {
        version: claim::CLAIM_VERSION,
        transaction_id: transaction_id.clone(),
        workspace: workspace_canonical.clone(),
        scope: Scope::Project,
        to: Scope::Personal,
        requested_slug: "note".into(),
        entry_slug: "note".into(),
        source_root,
        destination_root,
        relative: PathBuf::from("notes/note.md"),
        source: source.canonicalize().unwrap(),
        destination: destination.clone(),
        staging: staging_path(&destination, &transaction_id).unwrap(),
    };
    let claim_path = claim_path(&claim_root, &workspace_canonical, Scope::Project, Scope::Personal, "note");
    persist_claim(&claim_path, &claim).unwrap();
    validate_claim(&claim, &workspace_canonical, &home_canonical, &workspace, Scope::Project, Scope::Personal, "note").unwrap();
    transfer::fail_after_publish();
    assert!(execute_claim(&claim_path, &claim, true).unwrap_err().contains("injected"));
    assert!(claim.source.exists() && claim.destination.exists() && claim_path.exists());

    validate_claim(&claim, &workspace_canonical, &home_canonical, &workspace, Scope::Project, Scope::Personal, "note").unwrap();
    assert_eq!(PathBuf::from(execute_claim(&claim_path, &claim, true).unwrap()), destination);
    assert!(!claim.source.exists());
    assert!(!claim_path.exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn project_supplied_claim_cannot_move_a_different_personal_entry() {
    let (root, workspace, home) = fixture("adversarial");
    let requested = home.join(".agents/notes/requested.md");
    let private = home.join(".agents/notes/private.md");
    std::fs::write(&requested, "---\ndescription: requested\n---\nrequested\n").unwrap();
    std::fs::write(&private, "---\ndescription: private\n---\nsecret\n").unwrap();
    let malicious_root = workspace.join(".agents/.kxen-moves");
    std::fs::create_dir_all(&malicious_root).unwrap();
    let malicious = serde_json::json!({
        "version": 1,
        "source": private,
        "destination": workspace.join(".agents/notes/stolen.md"),
        "staging": workspace.join(".agents/notes/.stolen.kxen-move")
    });
    std::fs::write(malicious_root.join("attacker.json"), serde_json::to_vec(&malicious).unwrap()).unwrap();

    let moved =
        move_entry_with_roots(Scope::Personal, &workspace, &home, "requested", Scope::Project, &home.join("private-claims")).unwrap();
    assert!(std::fs::read_to_string(moved).unwrap().contains("requested"));
    assert!(private.exists(), "repo claim must not authorize access to another personal entry");
    assert!(!workspace.join(".agents/notes/stolen.md").exists());
    std::fs::remove_dir_all(root).ok();
}

#[cfg(unix)]
#[test]
fn symlinked_destination_parent_is_rejected_without_moving_source() {
    use std::os::unix::fs::symlink;

    let (root, workspace, home) = fixture("destination-symlink");
    let source = workspace.join(".agents/notes/note.md");
    std::fs::write(&source, "---\ndescription: note\n---\nsource\n").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir(home.join(".agents/notes")).unwrap();
    symlink(&outside, home.join(".agents/notes")).unwrap();

    let error =
        move_entry_with_roots(Scope::Project, &workspace, &home, "note", Scope::Personal, &home.join("private-claims")).unwrap_err();
    assert!(error.contains("symlink"), "{error}");
    assert!(source.exists());
    assert!(std::fs::read_dir(outside).unwrap().next().is_none());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn private_claim_with_parent_escape_is_rejected() {
    let (root, workspace, home) = fixture("claim-escape");
    let source = workspace.join(".agents/notes/note.md");
    std::fs::write(&source, "---\ndescription: note\n---\nsource\n").unwrap();
    let workspace_canonical = workspace.canonicalize().unwrap();
    let home_canonical = home.canonicalize().unwrap();
    let source_root = workspace.join(".agents").canonicalize().unwrap();
    let destination_root = home.join(".agents").canonicalize().unwrap();
    let claim = MoveClaim {
        version: claim::CLAIM_VERSION,
        transaction_id: "move_test_escape".into(),
        workspace: workspace_canonical.clone(),
        scope: Scope::Project,
        to: Scope::Personal,
        requested_slug: "note".into(),
        entry_slug: "note".into(),
        source_root,
        destination_root: destination_root.clone(),
        relative: PathBuf::from("../outside.md"),
        source: source.canonicalize().unwrap(),
        destination: destination_root.join("../outside.md"),
        staging: destination_root.join(".outside.move_test_escape.kxen-move"),
    };

    let error =
        validate_claim(&claim, &workspace_canonical, &home_canonical, &workspace, Scope::Project, Scope::Personal, "note").unwrap_err();
    assert!(error.contains("relative path is invalid"), "{error}");
    assert!(source.exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn retry_resyncs_both_parents_before_removing_claim() {
    for fail_source in [false, true] {
        let (root, workspace, home) = fixture(if fail_source { "source-fsync" } else { "destination-fsync" });
        let source = workspace.join(".agents/notes/note.md");
        std::fs::write(&source, "---\ndescription: note\n---\ndurable\n").unwrap();
        let claim_root = prepare_private_claim_root(&home.join("private")).unwrap();
        let workspace_canonical = workspace.canonicalize().unwrap();
        let source_root = workspace.join(".agents").canonicalize().unwrap();
        let destination_root = home.join(".agents").canonicalize().unwrap();
        let destination = destination_root.join("notes/note.md");
        let transaction_id = "move_test_fsync".to_string();
        let claim = MoveClaim {
            version: claim::CLAIM_VERSION,
            transaction_id: transaction_id.clone(),
            workspace: workspace_canonical.clone(),
            scope: Scope::Project,
            to: Scope::Personal,
            requested_slug: "note".into(),
            entry_slug: "note".into(),
            source_root,
            destination_root,
            relative: PathBuf::from("notes/note.md"),
            source: source.canonicalize().unwrap(),
            destination: destination.clone(),
            staging: staging_path(&destination, &transaction_id).unwrap(),
        };
        let claim_path = claim_path(&claim_root, &workspace_canonical, Scope::Project, Scope::Personal, "note");
        persist_claim(&claim_path, &claim).unwrap();
        let failed_parent = if fail_source { claim.source.parent().unwrap() } else { claim.destination.parent().unwrap() };
        path::fail_next_sync(failed_parent);
        assert!(execute_claim(&claim_path, &claim, false).is_err());
        assert!(claim_path.exists(), "fsync failure must retain the recovery claim");
        assert!(!claim.source.exists() && claim.destination.exists());

        execute_claim(&claim_path, &claim, false).unwrap();
        assert!(!claim_path.exists());
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn claim_parent_sync_failure_restores_retry_record() {
    let (root, workspace, home) = fixture("claim-fsync");
    let source = workspace.join(".agents/notes/note.md");
    std::fs::write(&source, "---\ndescription: note\n---\ndurable\n").unwrap();
    let claim_root = prepare_private_claim_root(&home.join("private")).unwrap();
    let workspace_canonical = workspace.canonicalize().unwrap();
    let destination_root = home.join(".agents").canonicalize().unwrap();
    let destination = destination_root.join("notes/note.md");
    let transaction_id = "move_test_claim_fsync".to_string();
    let claim = MoveClaim {
        version: claim::CLAIM_VERSION,
        transaction_id: transaction_id.clone(),
        workspace: workspace_canonical.clone(),
        scope: Scope::Project,
        to: Scope::Personal,
        requested_slug: "note".into(),
        entry_slug: "note".into(),
        source_root: workspace.join(".agents").canonicalize().unwrap(),
        destination_root,
        relative: PathBuf::from("notes/note.md"),
        source: source.canonicalize().unwrap(),
        destination: destination.clone(),
        staging: staging_path(&destination, &transaction_id).unwrap(),
    };
    let claim_path = claim_path(&claim_root, &workspace_canonical, Scope::Project, Scope::Personal, "note");
    persist_claim(&claim_path, &claim).unwrap();
    path::fail_next_sync(&claim_root);

    let error = execute_claim(&claim_path, &claim, false).unwrap_err();
    assert!(error.contains("claim restored for retry"), "{error}");
    assert!(claim_path.exists());
    assert!(!claim.source.exists() && claim.destination.exists());
    assert_eq!(PathBuf::from(execute_claim(&claim_path, &claim, false).unwrap()), destination);
    assert!(!claim_path.exists());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn stale_fixed_temp_from_old_version_does_not_wedge_new_claim() {
    let (root, workspace, home) = fixture("stale-temp");
    let claim_root = prepare_private_claim_root(&home.join("private")).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let path = claim_path(&claim_root, &workspace, Scope::Project, Scope::Personal, "note");
    std::fs::write(path.with_extension("json.tmp"), "stale").unwrap();
    let destination_root = home.join(".agents").canonicalize().unwrap();
    let source_root = root.join("source");
    std::fs::create_dir_all(&source_root).unwrap();
    let transaction_id = "move_test_temp".to_string();
    let destination = destination_root.join("notes/note.md");
    let claim = MoveClaim {
        version: claim::CLAIM_VERSION,
        transaction_id: transaction_id.clone(),
        workspace,
        scope: Scope::Project,
        to: Scope::Personal,
        requested_slug: "note".into(),
        entry_slug: "note".into(),
        source_root: source_root.clone(),
        destination_root,
        relative: PathBuf::from("notes/note.md"),
        source: source_root.join("notes/note.md"),
        destination: destination.clone(),
        staging: staging_path(&destination, &transaction_id).unwrap(),
    };
    begin_claim(&path, &claim).unwrap();
    assert!(path.is_file());
    assert!(begin_claim(&path, &claim).unwrap_err().contains("File exists"));
    std::fs::remove_dir_all(root).ok();
}

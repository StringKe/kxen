// fs_tool 公开 API 集成测试（从 fs_tool.rs 拆出，350 行门禁）：
// read 分页 / 文件新鲜度（纳秒精度）/ edit 双模式。
use kxen_app::tools::fs_tool::{AnchorEdit, EditSpec, FileTracker, FsToolError, edit, read, write};
use kxen_app::tools::hashline::generate_anchors;
use std::path::PathBuf;

fn temp_file(tag: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-fstool-{tag}-{}-{}", std::process::id(), rand()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.txt");
    std::fs::write(&path, content).unwrap();
    path
}

fn rand() -> u32 {
    // 纳秒 + 进程内序号混合：并行测试同纳秒也不再撞目录（flake 实证）
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    nanos ^ SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed).wrapping_mul(0x9e37)
}

// ---------------- read 分页 ----------------

#[test]
fn read_default_window_unchanged() {
    let body: String = (1..=10).map(|i| format!("line{i:02}\n")).collect();
    let path = temp_file("default", &body);
    let tracker = FileTracker::default();
    let r = read(&path, &tracker, "/tmp", None, None).unwrap();
    assert_eq!(r.total_lines, 10);
    assert_eq!(r.start_line, 1);
    assert_eq!(r.end_line, 10);
    assert!(!r.truncated);
    assert!(r.content.contains("line01") && r.content.contains("line10"));
}

#[test]
fn read_offset_limit_pages() {
    let body: String = (1..=10).map(|i| format!("line{i:02}\n")).collect();
    let path = temp_file("paging", &body);
    let tracker = FileTracker::default();

    let r = read(&path, &tracker, "/tmp", Some(3), Some(4)).unwrap();
    assert_eq!((r.start_line, r.end_line, r.total_lines), (3, 6, 10));
    assert!(r.truncated, "第 6 行后还有内容");
    assert!(r.content.contains("line03") && r.content.contains("line06"));
    assert!(!r.content.contains("line07"));

    // 尾段：truncated 为 false，agent 知道读完了
    let tail = read(&path, &tracker, "/tmp", Some(7), None).unwrap();
    assert_eq!((tail.start_line, tail.end_line), (7, 10));
    assert!(!tail.truncated);
    assert!(tail.content.contains("line10"));

    // offset 越界：空窗口（end_line < start_line），由调用侧出提示
    let beyond = read(&path, &tracker, "/tmp", Some(50), None).unwrap();
    assert!(beyond.end_line < beyond.start_line);
    assert!(beyond.content.is_empty());
}

#[test]
fn paged_anchors_work_for_edit() {
    // 分页读出的锚点基于全文计算，必须能直接用于锚点编辑（否则分页 read 会废掉 edit）
    let body: String = (1..=30).map(|i| format!("line{i:02}\n")).collect();
    let path = temp_file("anchors", &body);
    let tracker = FileTracker::default();

    let page = read(&path, &tracker, "/tmp", Some(21), Some(10)).unwrap();
    let line25 = page.content.lines().find(|l| l.contains("line25")).expect("line25 in page");
    let anchor = line25.split_whitespace().next().unwrap().trim().to_string();
    assert!(anchor.starts_with("25#"), "窗口内锚点须保留全文行号: {anchor}");

    let spec = EditSpec::Anchors { edits: vec![AnchorEdit { anchor, new_text: "LINE25".into() }] };
    assert_eq!(edit(&path, &spec, &tracker, "/tmp").unwrap().applied, 1);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("LINE25"));
    assert!(after.contains("line24") && after.contains("line26"), "只命中第 25 行");
}

// ---------------- 文件新鲜度 ----------------

#[test]
fn fresh_unchanged_and_size_change() {
    let path = temp_file("fresh", "hello\n");
    let tracker = FileTracker::default();
    assert!(!tracker.fresh(&path), "未 mark 的文件不算 fresh");
    tracker.mark(&path);
    assert!(tracker.fresh(&path));

    std::fs::write(&path, "hello world\n").unwrap();
    assert!(!tracker.fresh(&path), "size 变了必须检出");
}

#[test]
fn fresh_detects_same_second_same_size_rewrite() {
    // 秒级 mtime + size 会漏掉同秒同长度改写；纳秒精度下必须检出
    let path = temp_file("samesec", "AAAA\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    // 等 5ms 保证纳秒级 mtime 必然前进（APFS 为纳秒粒度）
    std::thread::sleep(std::time::Duration::from_millis(5));
    std::fs::write(&path, "BBBB\n").unwrap();
    assert!(!tracker.fresh(&path), "同秒同大小改写也必须检出");
}

// ---------------- edit（迁移自 fs_tool.rs 体内测试） ----------------

#[test]
fn anchor_edit_roundtrip() {
    let path = temp_file("roundtrip", "alpha\nbeta\ngamma\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    let lines: Vec<&str> = "alpha\nbeta\ngamma\n".lines().collect();
    let anchors = generate_anchors(&lines);
    let spec = EditSpec::Anchors { edits: vec![AnchorEdit { anchor: anchors[1].to_string(), new_text: "BETA".into() }] };
    let result = edit(&path, &spec, &tracker, "/tmp").unwrap();
    assert_eq!(result.applied, 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nBETA\ngamma\n");
}

#[test]
fn match_edit_ambiguous() {
    let path = temp_file("ambiguous", "x\nx\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    let spec = EditSpec::Match { old_string: "x".into(), new_string: "y".into(), expected_replacements: None };
    assert!(matches!(edit(&path, &spec, &tracker, "/tmp"), Err(FsToolError::Ambiguous { .. })));
}

// ---------------- edit 外部变更检测 ----------------

#[test]
fn anchor_edit_rejected_after_external_change() {
    // 外部（不经工具）改写后，旧锚点不可信：拒绝并提示重读，文件不被改动
    let path = temp_file("ext-anchor", "alpha\nbeta\ngamma\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    std::thread::sleep(std::time::Duration::from_millis(5)); // 保证纳秒 mtime 前进
    std::fs::write(&path, "alpha\nbeta\nGAMMA\n").unwrap();

    // "beta" 行内容未变，锚点本身仍命中：证明拦截发生在 tracker 层而非锚点层
    let lines: Vec<&str> = "alpha\nbeta\ngamma\n".lines().collect();
    let anchors = generate_anchors(&lines);
    let spec = EditSpec::Anchors { edits: vec![AnchorEdit { anchor: anchors[1].to_string(), new_text: "BETA".into() }] };
    let err = edit(&path, &spec, &tracker, "/tmp").unwrap_err();
    assert!(matches!(err, FsToolError::ExternallyModified { .. }), "{err}");
    assert!(err.to_string().contains("re-read"), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nbeta\nGAMMA\n");
}

#[test]
fn match_edit_self_heals_after_external_change() {
    // match 按内容匹配：外部变更后用新鲜内容直接应用，且写后 tracker 刷新不再误报
    let path = temp_file("ext-match", "alpha\nbeta\ngamma\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    std::thread::sleep(std::time::Duration::from_millis(5));
    std::fs::write(&path, "alpha\ninserted\nbeta\ngamma\n").unwrap();

    let spec = EditSpec::Match { old_string: "beta".into(), new_string: "BETA".into(), expected_replacements: None };
    assert_eq!(edit(&path, &spec, &tracker, "/tmp").unwrap().applied, 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ninserted\nBETA\ngamma\n");

    let lines: Vec<&str> = "alpha\ninserted\nBETA\ngamma\n".lines().collect();
    let anchors = generate_anchors(&lines);
    let spec = EditSpec::Anchors { edits: vec![AnchorEdit { anchor: anchors[2].to_string(), new_text: "beta2".into() }] };
    assert_eq!(edit(&path, &spec, &tracker, "/tmp").unwrap().applied, 1, "自愈后锚点 edit 不应再报外部变更");
}

#[test]
fn unchanged_edit_no_false_positive() {
    // 未变更：mtime 快路径判 fresh，anchors edit 直接放行
    let path = temp_file("ext-fresh", "alpha\nbeta\n");
    let tracker = FileTracker::default();
    tracker.mark(&path);
    assert!(!tracker.changed_externally(&path));
    let lines: Vec<&str> = "alpha\nbeta\n".lines().collect();
    let anchors = generate_anchors(&lines);
    let spec = EditSpec::Anchors { edits: vec![AnchorEdit { anchor: anchors[0].to_string(), new_text: "ALPHA".into() }] };
    assert_eq!(edit(&path, &spec, &tracker, "/tmp").unwrap().applied, 1);
}

// ---------------- write 覆盖备份（.kxen/backups/） ----------------

fn temp_workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kxen-fstool-{tag}-{}-{}", std::process::id(), rand()));
    std::fs::create_dir_all(&dir).unwrap();
    // canonicalize：macOS temp_dir 是 /var/folders（/private 的软链），
    // 非规范路径命中 safety F1 保护规则（/var），新建文件的 write 会被拒
    std::fs::canonicalize(&dir).unwrap()
}

/// 外部变更后覆盖：备份落 .kxen/backups/，工作区根目录无 .kxen-bak，.gitignore 含 .kxen/。
#[test]
fn write_backup_lands_in_kxen_dir() {
    let dir = temp_workspace("backup");
    let cwd = dir.to_string_lossy().to_string();
    let path = dir.join("test.txt");
    std::fs::write(&path, "old\n").unwrap();

    let tracker = FileTracker::default();
    tracker.mark(&path);
    std::thread::sleep(std::time::Duration::from_millis(5)); // 纳秒 mtime 前进，确保检出外部变更
    std::fs::write(&path, "externally changed\n").unwrap();
    write(&path, "new\n", &tracker, &cwd).unwrap();

    let backup = dir.join(".kxen/backups/test.kxen-bak");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "externally changed\n", "备份须保留覆盖前内容");
    assert!(!dir.join("test.kxen-bak").exists(), "工作区根目录不得出现 .kxen-bak");
    let gitignore = std::fs::read_to_string(dir.join(".gitignore")).unwrap();
    assert!(gitignore.lines().any(|l| l.trim() == ".kxen/"), ".gitignore 须含 .kxen/: {gitignore}");
}

/// 同名文件分处不同子目录：按相对路径镜像后两份备份互不覆盖。
#[test]
fn write_backup_mirrors_relative_path() {
    let dir = temp_workspace("backup-mirror");
    let cwd = dir.to_string_lossy().to_string();
    let tracker = FileTracker::default();

    for sub in ["a", "b"] {
        let path = dir.join(sub).join("same.txt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{sub}-old\n")).unwrap();
        tracker.mark(&path);
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&path, format!("{sub}-externally changed\n")).unwrap();
        write(&path, "new\n", &tracker, &cwd).unwrap();
    }

    let a = std::fs::read_to_string(dir.join(".kxen/backups/a/same.kxen-bak")).unwrap();
    let b = std::fs::read_to_string(dir.join(".kxen/backups/b/same.kxen-bak")).unwrap();
    assert_eq!(a, "a-externally changed\n");
    assert_eq!(b, "b-externally changed\n", "同名备份不得互相覆盖");
}

/// fresh 文件（本会话写过、无外部变更）覆盖不产生备份。
#[test]
fn write_fresh_file_no_backup() {
    let dir = temp_workspace("backup-fresh");
    let cwd = dir.to_string_lossy().to_string();
    let path = dir.join("test.txt");

    let tracker = FileTracker::default();
    write(&path, "v1\n", &tracker, &cwd).unwrap();
    write(&path, "v2\n", &tracker, &cwd).unwrap();
    assert!(!dir.join(".kxen").exists(), "无外部变更不得产生备份目录");
}

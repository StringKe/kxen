//! 写回路：notes/ 写入（同 slug 覆盖）、启停、双 scope 晋升、回收站删除、.kxen 私址存量迁移。

use super::scan::{scan_all, scan_all_with_home};
use super::{Entry, Kind, NOTE_TYPES, Scope, scope_root, slugify, today};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// 写入或更新一条 note（同 slug = 同题，整体覆盖不追加）。返回文件路径。
pub fn add(scope: Scope, workdir: &Path, slug: Option<&str>, note_type: &str, description: &str, content: &str) -> Result<String, String> {
    let note_type = if NOTE_TYPES.contains(&note_type) { note_type } else { "note" };
    let description = description.trim();
    if description.is_empty() {
        return Err("missing description".into());
    }
    let slug = slugify(slug.unwrap_or(description));
    let dir = scope_root(scope, workdir).join(Kind::Note.dir_name());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let body = format!("---\nnote-type: {note_type}\ndescription: {description}\ndate: {}\n---\n\n{}\n", today(), content.trim());
    let path = dir.join(format!("{slug}.md"));
    write_atomic(&path, body.as_bytes())?;
    Ok(path.to_string_lossy().into_owned())
}

/// 设置页与 knowledge 工具共用的全量列表（双 scope，scan 序 = 项目在前）。
pub fn list(workdir: &Path) -> Vec<Entry> {
    scan_all(workdir)
}

fn find_entry(scope: Scope, workdir: &Path, slug: &str) -> Result<Entry, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    find_entry_with_home(scope, workdir, &home, slug)
}

fn find_entry_with_home(scope: Scope, workdir: &Path, home: &Path, slug: &str) -> Result<Entry, String> {
    // 先精确匹配再回落 slugify 规范化：带哈希后缀的 CJK slug 二次 slugify 会追加新哈希而失真
    // （哈希取的是原始描述），规范化只兜底手输描述定位
    let entries = scan_all_with_home(workdir, home);
    let found = entries.iter().find(|e| e.scope == scope && e.slug == slug).or_else(|| {
        let normalized = slugify(slug);
        entries.iter().find(|e| e.scope == scope && e.slug == normalized)
    });
    found.cloned().ok_or_else(|| format!("not found: {}/{slug}", scope.as_str()))
}

/// 删除一条（进系统废纸篓可恢复；目录型 skill 整目录移走）。
pub fn remove(scope: Scope, workdir: &Path, slug: &str) -> Result<(), String> {
    let e = find_entry(scope, workdir, slug)?;
    let target = if !e.dir.is_empty() { PathBuf::from(&e.dir) } else { PathBuf::from(&e.path) };
    trash::delete(&target).map_err(|e| e.to_string())
}

/// 启停开关：frontmatter 加/去 enabled:false（注入跳过但不删除）。
pub fn set_enabled(scope: Scope, workdir: &Path, slug: &str, enabled: bool) -> Result<(), String> {
    let e = find_entry(scope, workdir, slug)?;
    let path = Path::new(&e.path);
    let lock = path_lock(path);
    let _guard = lock.lock().map_err(|error| error.to_string())?;
    let text = std::fs::read_to_string(&e.path).map_err(|err| err.to_string())?;
    let mut out = String::new();
    let mut seen = false;
    let mut in_fm = false;
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line == "---" {
            in_fm = true;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_fm && line == "---" {
            if !seen && !enabled {
                out.push_str("enabled: false\n");
            }
            in_fm = false;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if in_fm && line.starts_with("enabled:") {
            seen = true;
            if !enabled {
                out.push_str("enabled: false\n");
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    write_atomic_locked(path, out.as_bytes())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let lock = path_lock(path);
    let _guard = lock.lock().map_err(|error| error.to_string())?;
    write_atomic_locked(path, bytes)
}

fn write_atomic_locked(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| format!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let tmp = path.with_extension(format!("{}.tmp", path.extension().and_then(|value| value.to_str()).unwrap_or("file")));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    file.write_all(bytes).map_err(|error| format!("write {}: {error}", tmp.display()))?;
    if let Ok(metadata) = std::fs::metadata(path) {
        std::fs::set_permissions(&tmp, metadata.permissions()).map_err(|error| format!("chmod {}: {error}", tmp.display()))?;
    }
    file.sync_all().map_err(|error| format!("sync {}: {error}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|error| {
        std::fs::remove_file(&tmp).ok();
        format!("replace {}: {error}", path.display())
    })?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync {}: {error}", parent.display()))?;
    Ok(())
}

fn path_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    crate::core::shared::lock(LOCKS.get_or_init(|| Mutex::new(HashMap::new())))
        .entry(path.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// 跨 scope 晋升（personal -> project 唯一方向有意义，反向也允许）：保 kind 目录落位。
pub fn move_entry(scope: Scope, workdir: &Path, slug: &str, to: Scope) -> Result<String, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    move_entry_with_home(scope, workdir, &home, slug, to)
}

fn move_entry_with_home(scope: Scope, workdir: &Path, home: &Path, slug: &str, to: Scope) -> Result<String, String> {
    if scope == to {
        return Err("scope 相同".into());
    }
    let e = find_entry_with_home(scope, workdir, home, slug)?;
    if e.is_agents_md {
        return Err("root interoperability rule cannot move between scopes".into());
    }
    let source_root = scope_root_with_home(scope, workdir, home);
    let destination_root = scope_root_with_home(to, workdir, home);
    let source = if e.dir.is_empty() { PathBuf::from(&e.path) } else { PathBuf::from(&e.dir) };
    let relative = source.strip_prefix(&source_root).map_err(|_| format!("source is outside scope root: {}", source.display()))?;
    let dest = destination_root.join(relative);
    if dest.exists() {
        return Err(format!("destination already exists: {}", dest.display()));
    }
    let parent = dest.parent().ok_or_else(|| format!("destination has no parent: {}", dest.display()))?;
    std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let source_lock = path_lock(&source);
    let destination_lock = path_lock(&dest);
    let (_source_guard, _destination_guard) = if source <= dest {
        (source_lock.lock().map_err(|error| error.to_string())?, destination_lock.lock().map_err(|error| error.to_string())?)
    } else {
        let destination_guard = destination_lock.lock().map_err(|error| error.to_string())?;
        let source_guard = source_lock.lock().map_err(|error| error.to_string())?;
        (source_guard, destination_guard)
    };
    if dest.exists() {
        return Err(format!("destination already exists: {}", dest.display()));
    }
    std::fs::rename(&source, &dest).map_err(|err| err.to_string())?;
    Ok(dest.to_string_lossy().into_owned())
}

fn scope_root_with_home(scope: Scope, workdir: &Path, home: &Path) -> PathBuf {
    match scope {
        Scope::Project => workdir.join(".agents"),
        Scope::Personal => home.join(".agents"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-kn-store-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn same_slug_updates_not_duplicates() {
        let dir = ws("dedup");
        add(Scope::Project, &dir, None, "correction", "use trash not rm", "v1").unwrap();
        add(Scope::Project, &dir, None, "correction", "use trash not rm", "v2").unwrap();
        let entries: Vec<Entry> = list(&dir).into_iter().filter(|e| e.scope == Scope::Project && e.kind == Kind::Note).collect();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.contains("v2"));
        assert_eq!(entries[0].note_type.as_deref(), Some("correction"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_goes_to_trash() {
        let dir = ws("remove");
        let path = add(Scope::Project, &dir, None, "note", "temp note", "x").unwrap();
        remove(Scope::Project, &dir, "temp-note").unwrap();
        assert!(!Path::new(&path).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chinese_description_unique_locatable_slug() {
        let dir = ws("cjk");
        add(Scope::Project, &dir, None, "note", "修复登录页样式崩溃", "v1").unwrap();
        add(Scope::Project, &dir, None, "note", "修复登录页样式崩坏", "v2").unwrap();
        let entries: Vec<Entry> = list(&dir).into_iter().filter(|e| e.scope == Scope::Project && e.kind == Kind::Note).collect();
        assert_eq!(entries.len(), 2, "近义中文描述靠哈希后缀各自成条");
        assert_ne!(entries[0].slug, entries[1].slug);
        assert!(entries.iter().all(|e| e.slug.chars().any(crate::knowledge::is_cjk)), "slug 保留中文: {entries:?}");
        // 带哈希后缀的 slug 原样回传（UI 启停/删除的场景）：二次 slugify 会失真，必须精确命中
        let slug = entries[0].slug.clone();
        set_enabled(Scope::Project, &dir, &slug, false).unwrap();
        let after = list(&dir);
        assert!(!after.iter().find(|e| e.slug == slug).unwrap().enabled, "带哈希 slug 必须能定位启停");
        // 手输原始描述：回落 slugify 规范化（确定性哈希）同样定位
        set_enabled(Scope::Project, &dir, &entries[0].description, true).unwrap();
        let after2 = list(&dir);
        assert!(after2.iter().find(|e| e.slug == slug).unwrap().enabled);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_and_lookup_retain_shadowed_personal_entry() {
        let dir = ws("shadowed");
        let home = dir.join("fake-home");
        let project = dir.join(".agents/rules");
        let personal = home.join(".agents/rules");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::write(project.join("style.md"), "---\ndescription: project\n---\nproject\n").unwrap();
        std::fs::write(personal.join("style.md"), "---\ndescription: personal\n---\npersonal\n").unwrap();

        let entries = scan_all_with_home(&dir, &home);
        assert_eq!(entries.iter().filter(|entry| entry.kind == Kind::Rule && entry.slug == "style").count(), 2);
        let personal_entry = find_entry_with_home(Scope::Personal, &dir, &home, "style").unwrap();
        assert!(personal_entry.content.contains("personal"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn move_refuses_to_overwrite_shadowed_destination() {
        let dir = ws("move-conflict");
        let home = dir.join("fake-home");
        let project = dir.join(".agents/rules");
        let personal = home.join(".agents/rules");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&personal).unwrap();
        let project_path = project.join("style.md");
        let personal_path = personal.join("style.md");
        std::fs::write(&project_path, "---\ndescription: project\n---\nproject\n").unwrap();
        std::fs::write(&personal_path, "---\ndescription: personal\n---\npersonal\n").unwrap();

        let error = move_entry_with_home(Scope::Project, &dir, &home, "style", Scope::Personal).unwrap_err();
        assert!(error.contains("destination already exists"));
        assert!(std::fs::read_to_string(project_path).unwrap().contains("project"));
        assert!(std::fs::read_to_string(personal_path).unwrap().contains("personal"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn move_preserves_directory_skill_resources() {
        let dir = ws("move-skill");
        let home = dir.join("fake-home");
        let skill = dir.join(".agents/skills/review");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\ndescription: review\n---\nReview.\n").unwrap();
        std::fs::write(skill.join("checklist.md"), "resource\n").unwrap();

        let moved = move_entry_with_home(Scope::Project, &dir, &home, "review", Scope::Personal).unwrap();
        let destination = home.join(".agents/skills/review");
        assert_eq!(PathBuf::from(moved), destination);
        assert!(destination.join("SKILL.md").exists());
        assert!(destination.join("checklist.md").exists());
        assert!(!skill.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

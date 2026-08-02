//! 统一扫描：project 树在前 personal 树在后，同 (kind, slug) first-wins（项目覆盖个人）。
//! skills/ 特殊：目录型只认 SKILL.md（目录内其余 .md 是资源），扁平 .md 直接收。

use super::{Entry, Kind, Scope, parse::parse_entry};
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 8;
const MAX_FILE_BYTES: usize = 256 * 1024;
pub(super) const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;

pub fn scan(workdir: &Path) -> Vec<Entry> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    scan_with_home(workdir, &home)
}

/// home 抽参：测试用假 home，避免扫真实 ~/.agents。
pub(super) fn scan_with_home(workdir: &Path, home: &Path) -> Vec<Entry> {
    let mut unique = Vec::new();
    for entry in scan_all_with_home(workdir, home) {
        if !unique.iter().any(|current: &Entry| current.kind == entry.kind && current.slug == entry.slug) {
            unique.push(entry);
        }
    }
    unique
}

/// 管理视图必须保留双 scope 的同名条目；注入视图再按 project-first 做优先级去重。
pub(super) fn scan_all(workdir: &Path) -> Vec<Entry> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    scan_all_with_home(workdir, &home)
}

pub(super) fn scan_all_with_home(workdir: &Path, home: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut remaining = MAX_TOTAL_BYTES;
    let workspace_root = workdir.canonicalize().ok();
    // 根规则文件互操作：AGENTS.md 是主约定，CLAUDE.md/GEMINI.md/.cursorrules 同等注入
    if let Some(workspace_root) = workspace_root.as_deref() {
        for name in ["AGENTS.md", "CLAUDE.md", "GEMINI.md", ".cursorrules"] {
            let path = workdir.join(name);
            if let Some(text) = read_regular_utf8_within(&path, workspace_root, &mut remaining) {
                let mut e = parse_entry(Scope::Project, Kind::Rule, &path, &text);
                e.always_apply = true;
                e.is_agents_md = true;
                e.description = format!("root {name}");
                out.push(e);
            }
        }
    }
    walk(&workdir.join(".agents"), Scope::Project, &mut out, &mut remaining);
    walk(&home.join(".agents"), Scope::Personal, &mut out, &mut remaining);
    out
}

fn walk(root: &Path, scope: Scope, out: &mut Vec<Entry>, remaining: &mut usize) {
    let Ok(root_metadata) = std::fs::symlink_metadata(root) else {
        return;
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return;
    }
    let Ok(canonical_root) = root.canonicalize() else {
        return;
    };
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || *remaining == 0 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let Ok(canonical_path) = path.canonicalize() else {
                continue;
            };
            if !canonical_path.starts_with(&canonical_root) {
                continue;
            }
            if metadata.is_dir() {
                // 目录型 skill：收 SKILL.md 即停，不深入资源文件
                let skill_md = path.join("SKILL.md");
                if kind_of(root, &path) == Kind::Skill {
                    if let Some(text) = read_regular_utf8_within(&skill_md, &canonical_root, remaining) {
                        out.push(parse_entry(scope, Kind::Skill, &skill_md, &text));
                    } else {
                        stack.push((path, depth + 1));
                    }
                } else {
                    stack.push((path, depth + 1));
                }
            } else if metadata.is_file() && path.extension().is_some_and(|x| x == "md") {
                if path.file_name().is_some_and(|n| n == "SKILL.md") {
                    continue; // 已在目录分支处理
                }
                let kind = kind_of(root, &path);
                if let Some(text) = read_regular_utf8_within(&path, &canonical_root, remaining) {
                    let mut e = parse_entry(scope, kind, &path, &text);
                    // index.md 是该层目录的人工策展入口（渐进披露）：slug 带 scope 根相对路径，
                    // 多层目录各自的 index.md 不因同 slug 被 first-wins 去重吞掉；
                    // 落在 skills/ 下的 index.md 是入口不是 skill，不适用 skill 可见性规范
                    let is_index = path.file_name().is_some_and(|n| n == "index.md");
                    if is_index && let Ok(rel) = path.strip_prefix(root) {
                        e.slug = rel.with_extension("").to_string_lossy().into_owned();
                    }
                    // skill 无 description 不可被清单/调用发现，按规范跳过
                    if !is_index && e.kind == Kind::Skill && e.description.is_empty() {
                        continue;
                    }
                    out.push(e);
                }
            }
        }
    }
}

pub(super) fn read_regular_utf8_within(path: &Path, canonical_root: &Path, remaining: &mut usize) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    let canonical_path = path.canonicalize().ok()?;
    if !canonical_path.starts_with(canonical_root) {
        return None;
    }
    let declared_len = usize::try_from(metadata.len()).ok()?;
    if declared_len > MAX_FILE_BYTES || declared_len > *remaining {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let actual_len = text.len();
    if actual_len > MAX_FILE_BYTES || actual_len > *remaining {
        return None;
    }
    *remaining -= actual_len;
    Some(text)
}

/// kind 由 scope 根下第一级子目录推断；根散文件与未知子目录按 Reference（可被 frontmatter 覆盖）。
fn kind_of(root: &Path, path: &Path) -> Kind {
    let Some(first) = path.strip_prefix(root).ok().and_then(|rel| rel.components().next()).and_then(|c| c.as_os_str().to_str()) else {
        return Kind::Reference;
    };
    // 复数目录名优先按单数解析（rules->rule），失败再按原名（history 这类以 s 结尾的）
    Kind::from_str(first.trim_end_matches('s')).or_else(|| Kind::from_str(first)).unwrap_or(Kind::Reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-kn-scan-{tag}-{}", std::process::id()));
        let rules = dir.join(".agents/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("style.md"), "---\nalwaysApply: true\ndescription: 风格\n---\n用 trash。\n").unwrap();
        let skills = dir.join(".agents/skills/review");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("SKILL.md"), "---\ndescription: 对抗审查\n---\n审查 $1\n").unwrap();
        std::fs::write(skills.join("checklist.md"), "资源文件不应成为条目").unwrap();
        dir
    }

    #[test]
    fn kinds_from_subdirs_and_skill_dir_resource_skipped() {
        let dir = fixture("kinds");
        let entries = scan(&dir);
        assert!(entries.iter().any(|e| e.kind == Kind::Rule && e.slug == "style"));
        assert!(entries.iter().any(|e| e.kind == Kind::Skill && e.slug == "review"));
        assert!(!entries.iter().any(|e| e.slug == "checklist"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interop_root_rule_files() {
        let dir = std::env::temp_dir().join(format!("kxen-kn-interop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "claude 专属规则").unwrap();
        std::fs::write(dir.join(".cursorrules"), "cursor 专属规则").unwrap();
        let home = dir.join("fake-home");
        let entries = scan_with_home(&dir, &home);
        assert!(entries.iter().any(|e| e.is_agents_md && e.description.contains("CLAUDE.md")));
        assert!(entries.iter().any(|e| e.is_agents_md && e.description.contains(".cursorrules")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn project_wins_over_personal_same_slug() {
        let dir = fixture("wins");
        let home = dir.join("fake-home");
        let personal_rules = home.join(".agents/rules");
        std::fs::create_dir_all(&personal_rules).unwrap();
        std::fs::write(personal_rules.join("style.md"), "---\ndescription: 个人版\n---\n个人内容\n").unwrap();
        let entries = scan_with_home(&dir, &home);
        let styles: Vec<&Entry> = entries.iter().filter(|e| e.slug == "style").collect();
        assert_eq!(styles.len(), 1, "同 (kind, slug) first-wins 去重");
        assert_eq!(styles[0].scope, Scope::Project);
        assert!(styles[0].content.contains("用 trash。"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn management_scan_retains_both_scopes_for_same_slug() {
        let dir = fixture("all-scopes");
        let home = dir.join("fake-home");
        let personal_rules = home.join(".agents/rules");
        std::fs::create_dir_all(&personal_rules).unwrap();
        std::fs::write(personal_rules.join("style.md"), "---\ndescription: 个人版\n---\n个人内容\n").unwrap();
        let entries = scan_all_with_home(&dir, &home);
        let styles: Vec<&Entry> = entries.iter().filter(|e| e.kind == Kind::Rule && e.slug == "style").collect();
        assert_eq!(styles.len(), 2, "管理视图不得隐藏被 project 覆盖的 personal 条目");
        assert!(styles.iter().any(|e| e.scope == Scope::Project));
        assert!(styles.iter().any(|e| e.scope == Scope::Personal));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn index_md_recognized_per_directory_layer() {
        let dir = fixture("index");
        std::fs::write(dir.join(".agents/index.md"), "---\ndescription: 总入口\n---\n先看这里。\n").unwrap();
        std::fs::write(dir.join(".agents/rules/index.md"), "---\ndescription: rules 层入口\n---\n规则地图。\n").unwrap();
        let home = dir.join("fake-home");
        let entries = scan_with_home(&dir, &home);
        let idx: Vec<&Entry> = entries.iter().filter(|e| e.path.ends_with("index.md")).collect();
        assert_eq!(idx.len(), 2, "多层 index.md 不得被同 slug first-wins 去重: {idx:?}");
        assert!(idx.iter().any(|e| e.slug == "index"));
        assert!(idx.iter().any(|e| e.slug == "rules/index"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_oversized_files_are_never_scanned() {
        use std::os::unix::fs::symlink;

        let dir = fixture("safe-files");
        let home = dir.join("fake-home");
        let outside = dir.join("outside-secret.txt");
        std::fs::write(&outside, "AWS_SECRET_ACCESS_KEY=secret").unwrap();
        symlink(&outside, dir.join(".agents/rules/cloud.md")).unwrap();
        symlink(&outside, dir.join("AGENTS.md")).unwrap();
        std::fs::write(dir.join(".agents/rules/huge.md"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();

        let entries = scan_all_with_home(&dir, &home);
        assert!(!entries.iter().any(|entry| entry.slug == "cloud"));
        assert!(!entries.iter().any(|entry| entry.slug == "huge"));
        assert!(!entries.iter().any(|entry| entry.content.contains("AWS_SECRET_ACCESS_KEY")));
        std::fs::remove_dir_all(&dir).ok();
    }
}

//! 统一知识系统：OKF 单规范 + project/personal 双 scope。
//! 一棵树两个镜像：project = <workdir>/.agents/（入 git 共享），personal = ~/.agents/（跟人走）。
//! rules / references / skills / commands / notes / memory / history 都是 Entry，区别只在 kind 与激活方式。

pub mod consolidate;
pub mod distill;
pub mod embedding;
pub mod embedding_cache;
mod parse;
mod render;
pub mod retrieval;
mod scan;
mod store;

pub use render::render;
pub(crate) use render::render_with_runtime;
pub use scan::scan;
pub(crate) use store::add_observed;
pub use store::{add, list, move_entry, remove, set_enabled};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    Personal,
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope, String> {
        match s {
            "project" => Ok(Scope::Project),
            // global 是 personal 的旧名，外部输入一律归一
            "personal" | "global" => Ok(Scope::Personal),
            other => Err(format!("unknown scope: {other} (project|personal)")),
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Personal => "personal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Rule,
    Reference,
    Skill,
    Command,
    Note,
    Memory,
    History,
}

impl Kind {
    /// 字符串 -> Kind（自有解析器，不实现 FromStr：返回值是 Option 而非 Result，语义不同）。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Kind> {
        Some(match s {
            "rule" => Kind::Rule,
            "reference" | "doc" => Kind::Reference,
            "skill" => Kind::Skill,
            "command" => Kind::Command,
            "note" => Kind::Note,
            "memory" => Kind::Memory,
            "history" => Kind::History,
            _ => return None,
        })
    }
    pub fn dir_name(&self) -> &'static str {
        match self {
            Kind::Rule => "rules",
            Kind::Reference => "references",
            Kind::Skill => "skills",
            Kind::Command => "commands",
            Kind::Note => "notes",
            Kind::Memory => "memory",
            Kind::History => "history",
        }
    }
}

/// note/memory 的子类型（蒸馏与人工写入共用）。
pub const NOTE_TYPES: &[&str] = &["correction", "convention", "pitfall", "preference", "note"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub scope: Scope,
    pub kind: Kind,
    pub slug: String,
    pub description: String,
    pub content: String,
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub always_apply: bool,
    #[serde(default)]
    pub globs: Vec<String>,
    /// skill/command 懒加载依赖：加载或展开时随正文注入的条目 slug。
    #[serde(default)]
    pub needs: Vec<String>,
    // skill 字段
    #[serde(default)]
    pub when_to_use: Option<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub disable_model_invocation: bool,
    #[serde(default = "default_true")]
    pub user_invocable: bool,
    // command 字段
    #[serde(default)]
    pub argument_hint: Option<String>,
    // note/memory 字段
    #[serde(default)]
    pub note_type: Option<String>,
    #[serde(default)]
    pub date: String,
    /// 目录型 skill 的资源目录（SKILL.md 的父目录）。
    #[serde(default)]
    pub dir: String,
    /// 根/就近 AGENTS.md 合成的条目（不在 kind 子目录内）。
    #[serde(default)]
    pub is_agents_md: bool,
}

fn default_true() -> bool {
    true
}

pub fn scope_root(scope: Scope, workdir: &Path) -> PathBuf {
    match scope {
        Scope::Project => workdir.join(".agents"),
        Scope::Personal => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty")).join(".agents"),
    }
}

/// CJK 表意文字：基本集 + 扩展 A + 兼容集（中文标点不在其内，走折叠分支）。
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}')
}

pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut dash = true;
    let mut has_cjk = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if is_cjk(c) {
            has_cjk = true;
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let truncated = trimmed.chars().count() > 48;
    let capped: String = trimmed.chars().take(48).collect();
    let capped = capped.trim_end_matches('-');
    if capped.is_empty() {
        return "note".to_string();
    }
    if !has_cjk && !truncated {
        return capped.to_string();
    }
    // CJK slug 追加哈希后缀（取未截断原文的 sha256 前 4 字节）：同文同 slug（覆盖写可定位），
    // 截断后同前缀的长标题不撞名；纯 ASCII 路径保持逐字符不变，无后缀
    use sha2::Digest;
    let digest = sha2::Sha256::digest(text.as_bytes());
    format!("{capped}-{:02x}{:02x}{:02x}{:02x}", digest[0], digest[1], digest[2], digest[3])
}

pub fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", now.year(), now.month() as u8, now.day())
}

/// needs 解析：跨双 scope 按 slug 找条目并渲染成注入块（project 优先，与 scan 去重同序）。
pub fn resolve_needs(workdir: &Path, needs: &[String]) -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/var/empty"));
    resolve_needs_inner(workdir, &home, needs)
}

fn resolve_needs_inner(workdir: &Path, home: &Path, needs: &[String]) -> String {
    if needs.is_empty() {
        return String::new();
    }
    let trusted = crate::core::trust::is_trusted(workdir);
    let entries = scan::scan_with_home(workdir, home);
    let mut out = String::from("\n<knowledge-deps>\n");
    let mut hit = 0;
    for need in needs {
        let slug = slugify(need);
        // 信任门：needs 正文随加载注入提示词，未信任项目 scope 的依赖条目跳过（personal 不受影响）
        if let Some(e) = entries.iter().find(|e| e.enabled && e.slug == slug && (trusted || e.scope != Scope::Project)) {
            hit += 1;
            out.push_str(&format!("## [{}] {}\n{}\n\n", e.kind.dir_name(), e.description, e.content.trim()));
        }
    }
    if hit == 0 {
        return String::new();
    }
    out.push_str("</knowledge-deps>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 进程级隔离信任 store：与 render 测试同值（Once 写序防并行 env 竞态）。
    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("KXEN_TRUST_FILE", std::env::temp_dir().join(format!("kxen-kn-trust-store-{}.json", std::process::id())));
        });
    }

    fn needs_fixture(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("kxen-kn-needs-{tag}-{}", std::process::id()));
        let home = dir.join("fake-home");
        let rules = dir.join(".agents/rules");
        std::fs::create_dir_all(&rules).unwrap();
        std::fs::write(rules.join("style-guide.md"), "---\ndescription: 风格\n---\n用 trash 不用 rm。\n").unwrap();
        (dir, home)
    }

    #[test]
    fn needs_resolve_injects_dep_bodies() {
        setup();
        let (dir, home) = needs_fixture("basic");
        crate::core::trust::trust(&dir).unwrap(); // 生产语义：未信任项目 scope 的依赖跳过，夹具显式信任
        let block = resolve_needs_inner(&dir, &home, &["style-guide".into(), "missing".into()]);
        assert!(block.contains("<knowledge-deps>"));
        assert!(block.contains("用 trash 不用 rm。"));
        assert!(resolve_needs_inner(&dir, &home, &["missing".into()]).is_empty());
        assert!(resolve_needs_inner(&dir, &home, &[]).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn needs_skip_untrusted_project_scope() {
        setup();
        let (dir, home) = needs_fixture("gated");
        assert!(resolve_needs_inner(&dir, &home, &["style-guide".into()]).is_empty(), "未信任项目 scope 的依赖条目不得注入");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slugify_keeps_cjk_and_uniquifies() {
        // 纯 ASCII 逐字符不变，无哈希后缀
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("fix  login"), "fix-login");
        // CJK 保留 + 中文标点折叠为 - + 8 位 hex 后缀
        let a = slugify("修复登录页样式崩溃");
        assert!(a.starts_with("修复登录页样式崩溃-"), "CJK 应保留: {a}");
        assert_eq!(a.len(), "修复登录页样式崩溃-".len() + 8, "sha256 前 4 字节 hex: {a}");
        let b = slugify("修复：登录问题");
        assert!(b.starts_with("修复-登录问题-"), "中文标点折叠为 -: {b}");
        // 确定性：同文同 slug；异题不同名
        assert_eq!(a, slugify("修复登录页样式崩溃"));
        assert_ne!(a, slugify("修复登录页样式崩坏"));
        // 全标点回落不变
        assert_eq!(slugify("！！！"), "note");
    }

    #[test]
    fn slugify_long_chinese_truncation_still_unique() {
        // 两个长中文题共享 >48 字符前缀：截断撞前缀，哈希后缀（取未截断原文）保证唯一
        let prefix = "项目记忆蒸馏的中文长标题需要超过四十八个字符才能触发截断逻辑甲乙丙丁戊己庚辛壬癸子丑寅卯辰巳午未申酉";
        let a = slugify(&format!("{prefix}结尾一"));
        let b = slugify(&format!("{prefix}结尾二"));
        assert_ne!(a, b, "截断后同前缀的长标题必须靠哈希区分");
        for s in [&a, &b] {
            // 48 截断上限不变：slug 体 <= 48 字符 + '-' + 8 hex
            let body = s.rsplit_once('-').map(|(b, _)| b).unwrap();
            assert!(body.chars().count() <= 48, "slug 体截断上限: {s}");
            assert!(!body.ends_with('-'), "截断残尾不收 -: {s}");
        }
    }

    #[test]
    fn slugify_long_ascii_truncation_still_unique() {
        let prefix = "a".repeat(55);
        let first = slugify(&format!("{prefix}-first"));
        let second = slugify(&format!("{prefix}-second"));
        assert_ne!(first, second);
        assert!(first.len() > 48 && second.len() > 48);
        assert_eq!(slugify("short-ascii-note"), "short-ascii-note", "short ASCII slugs keep compatibility");
    }
}

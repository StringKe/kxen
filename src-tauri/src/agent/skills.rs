//! skills 视图：统一知识系统 kind=Skill 条目的适配层（扫描与解析已并入 knowledge）。
//! 保留语义：递归深度 cap 3、$ARGUMENTS 展开、项目覆盖个人同名 first-wins（scan 序保证）。

use crate::knowledge::{self, Entry, Kind, Scope};
use std::path::Path;

pub const SKILL_RECURSION_CAP: u32 = 3;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub arguments: Vec<String>,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
    pub dir: String,
    pub content: String,
    pub needs: Vec<String>,
}

impl From<Entry> for Skill {
    fn from(e: Entry) -> Skill {
        Skill {
            name: e.slug,
            description: e.description,
            when_to_use: e.when_to_use,
            arguments: e.arguments,
            disable_model_invocation: e.disable_model_invocation,
            user_invocable: e.user_invocable,
            dir: e.dir,
            content: e.content,
            needs: e.needs,
        }
    }
}

pub fn scan(workdir: &Path) -> Vec<Skill> {
    let trusted = crate::core::trust::is_trusted(workdir);
    let mut skills: Vec<Skill> = knowledge::scan(workdir)
        .into_iter()
        // 信任门：skill 加载即提示词注入面，未信任项目的不进清单；personal 跟人走不受影响
        .filter(|e| e.kind == Kind::Skill && e.enabled && (trusted || e.scope != Scope::Project))
        .map(Skill::from)
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn find(workdir: &Path, name: &str) -> Option<Skill> {
    scan(workdir).into_iter().find(|s| s.name == name)
}

/// $ARGUMENTS / $1..$n / $ARGUMENTS[i] 展开；无占位符时尾部追加（kimi-code 同款行为）。
pub fn expand_args(content: &str, args: &str, declared: &[String]) -> String {
    let mut out = content.to_string();
    let raw_args: Vec<&str> = args.split_whitespace().collect();
    if out.contains("$ARGUMENTS") {
        out = out.replace("$ARGUMENTS", args);
    }
    for (i, arg) in raw_args.iter().enumerate() {
        out = out.replace(&format!("${}", i + 1), arg);
        out = out.replace(&format!("$ARGUMENTS[{i}]"), arg);
    }
    if (!out.contains('$') || (!content.contains("$ARGUMENTS") && !declared.is_empty()))
        && !args.is_empty()
        && !content.contains("$ARGUMENTS")
    {
        out.push_str(&format!("\nARGUMENTS: {args}"));
    }
    out
}

/// 装载：$ARGUMENTS 展开 + needs 依赖注入 + 统一包装（调研 §2 形态）。
pub fn render_loaded(skill: &Skill, args: &str, trigger: &str, deps: &str) -> String {
    let content = expand_args(&skill.content, args, &skill.arguments);
    format!(
        "<kxen-skill-loaded name=\"{}\" trigger=\"{trigger}\" dir=\"{}\" args=\"{args}\">\n{content}\n{deps}</kxen-skill-loaded>",
        skill.name, skill.dir
    )
}

/// skill 工具调用入口（agent_loop 执行路由拆出）：递归 cap + 同 args 重调拒绝 + 装载渲染。
pub fn invoke(workdir: &Path, extras: &crate::agent::agent_loop::SessionExtras, name: &str, args: &str) -> Result<String, String> {
    // 递归深度 cap 3（skill -> skill 链）
    let depth = extras.skill_depth.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let result = (|| {
        if depth > SKILL_RECURSION_CAP {
            return Err(format!("skill recursion cap ({}) reached", SKILL_RECURSION_CAP));
        }
        let Some(skill) = find(workdir, name) else {
            return Err(format!("skill not found: {name}"));
        };
        if skill.disable_model_invocation {
            return Err(format!("skill {name} is user-invocable only (disable-model-invocation)"));
        }
        // 同 args 禁止重调
        let key = format!("{name}\x1f{args}");
        if !crate::core::shared::lock(&extras.loaded_skills).insert(key) {
            return Err(format!("skill {name} already loaded with identical args - reuse the block in this session"));
        }
        let deps = crate::knowledge::resolve_needs(workdir, &skill.needs);
        Ok(render_loaded(&skill, args, "model", &deps))
    })();
    extras.skill_depth.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 进程级隔离信任 store：与 render 测试同值（Once 写序防并行 env 竞态）。
    fn setup() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            std::env::set_var("KXEN_TRUST_FILE", std::env::temp_dir().join(format!("kxen-kn-trust-store-{}.json", std::process::id())));
        });
    }

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-skills-{tag}-{}", std::process::id()));
        let flat = dir.join(".agents/skills");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(
            flat.join("commit.md"),
            "---\nname: commit\ndescription: Conventional Commits 提交助手\nwhen_to_use: 提交代码时\n---\n请按规范提交：$ARGUMENTS\n",
        )
        .unwrap();
        let nested = dir.join(".agents/skills/review");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("SKILL.md"), "---\ndescription: 对抗性审查\n---\n审查 $1 的改动。\n").unwrap();
        dir
    }

    #[test]
    fn scan_flat_and_nested() {
        setup();
        let dir = fixture("scan");
        crate::core::trust::trust(&dir).unwrap(); // 生产语义：未信任项目 skill 不进清单，夹具显式信任
        let skills = scan(&dir);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"commit"));
        assert!(names.contains(&"review"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn untrusted_project_skills_not_listed() {
        setup();
        let dir = fixture("untrusted");
        assert!(scan(&dir).is_empty(), "未信任项目的 skill 不得进清单");
        assert!(find(&dir, "commit").is_none(), "未信任项目的 skill 不可加载");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn arguments_expansion() {
        setup();
        let dir = fixture("args");
        crate::core::trust::trust(&dir).unwrap();
        let skill = find(&dir, "commit").unwrap();
        let loaded = render_loaded(&skill, "fix login bug", "user", "");
        assert!(loaded.contains("请按规范提交：fix login bug"));
        assert!(loaded.contains("name=\"commit\""));
        assert!(loaded.contains("trigger=\"user\""));

        let review = find(&dir, "review").unwrap();
        let loaded2 = render_loaded(&review, "src/auth", "model", "");
        assert!(loaded2.contains("审查 src/auth 的改动"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_description_is_skipped() {
        setup();
        let dir = std::env::temp_dir().join(format!("kxen-skills-bad-{}", std::process::id()));
        crate::core::trust::trust(&dir).unwrap();
        let flat = dir.join(".agents/skills");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("nodesc.md"), "---\nname: nodesc\n---\nbody\n").unwrap();
        assert!(scan(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}

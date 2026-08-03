//! / 命令：builtin + 统一知识系统 kind=Command 条目（模板正文 + argument-hint + needs 懒加载）。

use crate::knowledge::{self, Kind};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub kind: &'static str, // "builtin" | "custom" | "skill"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

const BUILTIN: &[(&str, &str, Option<&str>)] = &[
    ("write-goal", "交互式定义一个带完成判据的 goal", Some("<目标描述>")),
    ("ultracode", "大任务模式：分解 -> workflow 并行实现 -> 集成验证", Some("<实现任务>")),
    ("ultraplan", "多角度规划模式：架构/调研/风险并行 -> 综合成稿", Some("<规划问题>")),
    ("ultrareview", "对抗性多镜审查：正确性/安全/性能/约定", Some("<路径或范围>")),
    ("compact", "手动压缩当前 Session 历史", None),
    ("doctor", "环境自检（订阅凭证/目录/配置）", None),
];

/// command.list 数据源：builtin + custom（skills 由调用方拼）。
pub fn list(workdir: &Path) -> Vec<CommandInfo> {
    let mut out: Vec<CommandInfo> = BUILTIN
        .iter()
        .map(|(name, desc, hint)| CommandInfo {
            name: name.to_string(),
            description: desc.to_string(),
            kind: "builtin",
            argument_hint: hint.map(String::from),
        })
        .collect();
    out.extend(knowledge::scan(workdir).into_iter().filter(|e| e.kind == Kind::Command && e.enabled).map(|e| CommandInfo {
        name: e.slug,
        description: e.description,
        kind: "custom",
        argument_hint: e.argument_hint,
    }));
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 发送时展开自定义命令：$ARGUMENTS 模板 + needs 依赖懒加载注入。非自定义命令返回 None。
pub fn expand(workdir: &Path, name: &str, args: &str) -> Option<String> {
    let entry = knowledge::scan(workdir).into_iter().find(|e| e.kind == Kind::Command && e.enabled && e.slug == name)?;
    // 信任门：未信任项目 command 只索引（list 可见）不展开，与 render 的 untrusted downgrade 同模式
    if entry.scope == knowledge::Scope::Project && !crate::core::trust::is_trusted(workdir) {
        return None;
    }
    let content = crate::agent::skills::expand_args(&entry.content, args, &[]);
    let deps = knowledge::resolve_needs(workdir, &entry.needs);
    Some(format!("{content}\n{deps}"))
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

    fn fixture(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kxen-cmd-{tag}-{}", std::process::id()));
        let cmds = dir.join(".agents/commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(cmds.join("review.md"), "---\ndescription: 审查指定路径\nargument-hint: <路径>\n---\n审查 $ARGUMENTS\n").unwrap();
        dir
    }

    #[test]
    fn builtin_and_custom() {
        setup();
        let dir = fixture("list");
        crate::core::trust::trust(&dir).unwrap(); // 生产语义：夹具显式信任（未信任只索引）
        let list = list(&dir);
        assert!(list.iter().any(|c| c.name == "write-goal" && c.kind == "builtin"));
        for name in ["compact", "doctor"] {
            assert!(list.iter().any(|c| c.name == name && c.kind == "builtin"));
        }
        for name in ["clear", "model", "abort"] {
            assert!(!list.iter().any(|c| c.name == name), "{name} 不应伪装成可执行 builtin");
        }
        let compact = list.iter().find(|c| c.name == "compact").unwrap();
        assert_eq!(compact.description, "手动压缩当前 Session 历史");
        assert_eq!(compact.argument_hint, None);
        let custom = list.iter().find(|c| c.name == "review").unwrap();
        assert_eq!(custom.kind, "custom");
        assert_eq!(custom.description, "审查指定路径");
        assert_eq!(custom.argument_hint.as_deref(), Some("<路径>"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn expand_template_with_args() {
        setup();
        let dir = fixture("expand");
        crate::core::trust::trust(&dir).unwrap();
        let out = expand(&dir, "review", "src/auth").unwrap();
        assert!(out.contains("审查 src/auth"));
        assert!(expand(&dir, "nonexistent", "").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn untrusted_command_indexed_but_not_expanded() {
        setup();
        let dir = fixture("untrusted");
        assert!(list(&dir).iter().any(|c| c.name == "review"), "未信任项目 command 仍索引可见");
        assert!(expand(&dir, "review", "src/auth").is_none(), "未信任项目 command 不展开");
        std::fs::remove_dir_all(&dir).ok();
    }
}

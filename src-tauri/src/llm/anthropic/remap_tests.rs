use super::{remap_tool_name, unmap_tool_name};

#[test]
fn tool_remap_full_table_roundtrip() {
    // 全表往返：kxen 名 -> Claude 名 -> kxen 名必须恒等（Claude 名以 OAuth 契约习惯名为准）
    for (kxen, claude) in [
        ("exec", "Bash"),
        ("read", "Read"),
        ("write", "Write"),
        ("edit", "Edit"),
        ("glob", "Glob"),
        ("grep", "Grep"),
        ("agent", "Agent"),
        ("schedule", "ScheduleWakeup"),
        ("skill", "Skill"),
    ] {
        assert_eq!(remap_tool_name(kxen), claude, "{kxen} 正映射");
        assert_eq!(unmap_tool_name(claude), kxen, "{claude} 逆映射");
    }
}

#[test]
fn tool_remap_arms_point_at_real_tool_names() {
    // 映射臂必须指向真实工具名（tools_spec/tools_deferred 派发名）：模型回的 "Agent"/"Skill"
    // 逆映射成不存在的名字会在执行层撞 unknown tool
    assert_eq!(unmap_tool_name("Agent"), "agent");
    assert_eq!(unmap_tool_name("Skill"), "skill");
    // 旧工具名不再是映射源：不应被占用成 Claude 名（直传等于把错误暴露给 API）
    assert_eq!(remap_tool_name("subagent"), "subagent");
    assert_eq!(remap_tool_name("skill_manage"), "skill_manage");
}

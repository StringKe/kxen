// safety 命令评估与路径守卫测试（从 eval.rs 拆出，350 行门禁）。
use kxen_app::tools::safety::{Verdict, evaluate_shell_command, guard_path};

const CWD: &str = "/Users/test/project";

fn denied(cmd: &str) -> bool {
    matches!(evaluate_shell_command(cmd, CWD), Verdict::Deny { .. })
}

fn allowed(cmd: &str) -> bool {
    matches!(evaluate_shell_command(cmd, CWD), Verdict::Allow | Verdict::Recoverable)
}

#[test]
fn f1_system() {
    for cmd in [
        "rm -rf /",
        "rm -rf /usr",
        "sudo rm -rf /etc",
        "dd if=/dev/zero of=/dev/disk0",
        "mkfs.ext4 /dev/sda1",
        "diskutil eraseDisk JHFS+ New disk0",
        "find / -name x -delete",
    ] {
        assert!(denied(cmd), "should deny: {cmd}");
    }
}

#[test]
fn macos_temp_exempt() {
    assert!(allowed("rm -rf /private/var/folders/qb/xxx/T/test"));
    assert!(allowed("rm -rf /private/tmp/foo"));
    assert!(allowed("rm -rf /tmp/foo"));
    assert!(denied("rm -rf /private/etc"));
    assert!(denied("rm -rf /private/var/db"));
}

#[test]
fn separators_and_substitutions() {
    // || 与换行同样切段
    assert!(denied("ls || rm -rf /private/etc"));
    assert!(denied("ls\nrm -rf /private/etc"));
    // 反引号 / $() 内嵌命令纳入评估
    assert!(denied("echo $(rm -rf /private/etc)"));
    assert!(denied("echo `rm -rf /private/etc`"));
    assert!(allowed("echo $(ls -la)"));
}

#[test]
fn ask_verdict() {
    assert!(matches!(evaluate_shell_command("git push --force origin main", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("git push -f origin main", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("git reset --hard", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("sudo apt install jq", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("git clean -fd", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("kill -9 1234", CWD), Verdict::Ask { .. }));
    assert!(matches!(evaluate_shell_command("brew uninstall node", CWD), Verdict::Ask { .. }));
    // 带 ref 的 reset --hard 是常用安全操作，不进审批
    assert!(allowed("git reset --hard HEAD"));
    // 具体危险优先于审批：sudo rm -rf /etc 仍是 Deny 不是 Ask
    assert!(matches!(evaluate_shell_command("sudo rm -rf /etc", CWD), Verdict::Deny { .. }));
}

#[test]
fn f2_home() {
    assert!(denied("rm -rf ~"));
    assert!(denied("rm -rf ~/Documents"));
    assert!(denied("trash ~/.ssh"));
    assert!(allowed("rm ~/Documents/draft.txt"));
}

#[test]
fn f3_git() {
    assert!(denied("rm -rf .git"));
    assert!(denied("mv .git /tmp/trash"));
    assert!(denied("git update-ref -d refs/heads/main"));
    assert!(allowed("git reset --hard HEAD"));
    assert!(allowed("git branch -d feature-x"));
}

#[test]
fn f4_destroy() {
    for cmd in
        ["terraform destroy", "dropdb production", "kubectl delete ns prod", "aws s3 rb s3://b --force", "docker system prune --volumes"]
    {
        assert!(denied(cmd), "should deny: {cmd}");
    }
}

#[test]
fn f5_bypass() {
    assert!(denied("bash -c \"rm -rf /usr\""));
    assert!(denied("rm -rf $DIR/"));
}

#[test]
fn env_assignment_prefix_does_not_bypass() {
    // 前导 VAR=value 赋值不是命令本身：跳过赋值后的真命令照样进删除判定
    assert!(denied("X=1 rm -rf ~"));
    assert!(denied("A=1 B=2 rm -rf /private/etc"));
    assert!(denied("sudo X=1 rm -rf /usr"));
    assert!(allowed("X=1 ls -la"));
}

#[test]
fn nested_substitutions_are_evaluated() {
    // 平衡括号：嵌套 $() 的内层命令同样进评估（非嵌套正则只捕到残缺外层）
    assert!(denied("echo $(cat $(rm -rf /private/etc))"));
    assert!(denied("echo $(ls $(rm -rf ~))"));
    assert!(allowed("echo $(ls $(pwd))"));
}

#[test]
fn f2_credential_list_matches_path_policy() {
    // 与 path_policy::sensitive_reason 同一清单：任一漏项都是删除凭证的绕过通道
    for dot in [".ssh", ".gnupg", ".aws", ".kube", ".docker", ".codex", ".claude", ".grok", ".kimi-code"] {
        assert!(denied(&format!("rm -rf ~/{dot}")), "~/{dot} 应被拒绝");
    }
}

#[test]
fn trash_recoverable() {
    assert!(matches!(evaluate_shell_command("trash ./dist", CWD), Verdict::Recoverable));
    assert!(denied("trash .git"));
    assert!(denied("trash ~/.ssh"));
}

#[test]
fn guard() {
    assert!(matches!(guard_path("~/.ssh/id_rsa", CWD), Verdict::Deny { .. }));
    assert!(matches!(guard_path(".git/config", CWD), Verdict::Deny { .. }));
    assert!(matches!(guard_path("src/index.ts", CWD), Verdict::Allow));
}

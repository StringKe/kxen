//! validate_dialect 方言陷阱规则测试。
//! 每条规则覆盖命中（拒绝 + 纠正文案）与误报（正确写法放行）。

use kxen_app::tools::exec::validate_dialect;
use kxen_app::tools::shell::ShellKind;

fn rejected(kind: ShellKind, cmd: &str) -> String {
    validate_dialect(kind, cmd).expect_err(&format!("应拒绝: {cmd}")).to_string()
}

#[test]
fn zsh_zero_index_rejected() {
    assert!(rejected(ShellKind::Zsh, "echo ${arr[0]}").contains("1-indexed"));
    // 误报：1 基下标与非数组 [0] 之外的用法放行
    assert!(validate_dialect(ShellKind::Zsh, "echo ${arr[1]}").is_ok());
}

#[test]
fn zsh_bash_case_expansion_rejected() {
    assert!(rejected(ShellKind::Zsh, "echo ${name,,}").contains("${(L)var}"));
    assert!(rejected(ShellKind::Zsh, "echo ${name,}").contains("${(L)var}"));
    // 误报：zsh 自己的写法放行
    assert!(validate_dialect(ShellKind::Zsh, "echo ${(L)name}").is_ok());
}

#[test]
fn zsh_equals_cmd_expansion_rejected() {
    assert!(rejected(ShellKind::Zsh, "=ls -la").contains("=cmd"));
    assert!(rejected(ShellKind::Zsh, "cp =which /tmp").contains("=cmd"));
    // 误报：赋值与 key=value 参数不放行成冤案
    assert!(validate_dialect(ShellKind::Zsh, "FOO=bar echo ok").is_ok());
    assert!(validate_dialect(ShellKind::Zsh, "curl -d a=b https://example.com").is_ok());
}

#[test]
fn zsh_word_split_difference_rejected() {
    assert!(rejected(ShellKind::Zsh, "for f in $files; do echo $f; done").contains("sh_word_split"));
    assert!(rejected(ShellKind::Zsh, "for f in ${files}; do :; done").contains("sh_word_split"));
    // 误报：数组遍历与 ${=var} 显式拆分放行
    assert!(validate_dialect(ShellKind::Zsh, "for f in $files[@]; do :; done").is_ok());
    assert!(validate_dialect(ShellKind::Zsh, "for f in ${=files}; do :; done").is_ok());
}

#[test]
fn bash_mapfile_rejected_on_macos_32() {
    assert!(rejected(ShellKind::Bash, "mapfile -t arr < <(ls)").contains("bash 3.2"));
    assert!(rejected(ShellKind::Bash, "readarray arr < f.txt").contains("bash 3.2"));
    // 误报：while read 替代写法放行
    assert!(validate_dialect(ShellKind::Bash, "while IFS= read -r l; do echo $l; done < f.txt").is_ok());
}

#[test]
fn fish_export_rejected() {
    assert!(rejected(ShellKind::Fish, "export PATH=$PATH:/x").contains("set -x"));
    assert!(validate_dialect(ShellKind::Fish, "set -x PATH $PATH /x").is_ok());
}

#[test]
fn fish_dollar_paren_substitution_rejected() {
    assert!(rejected(ShellKind::Fish, "echo $(date)").contains("(...)"));
    // 误报：fish 原生 (...) 替换放行
    assert!(validate_dialect(ShellKind::Fish, "echo (date)").is_ok());
}

#[test]
fn fish_and_and_rejected() {
    assert!(rejected(ShellKind::Fish, "cd /tmp && ls").contains("; and"));
    // 误报：fish 的 ; and 写法放行
    assert!(validate_dialect(ShellKind::Fish, "cd /tmp; and ls").is_ok());
}

#[test]
fn fish_dollar_question_rejected() {
    assert!(rejected(ShellKind::Fish, "echo $?").contains("$status"));
    assert!(validate_dialect(ShellKind::Fish, "echo $status").is_ok());
}

#[test]
fn rules_scoped_to_declared_dialect() {
    // 同一条命令在声明的方言里合法即放行：zsh 的 ${(L)} 不被 bash 规则误伤，bash 的 $( ) 不被 fish 规则误伤
    assert!(validate_dialect(ShellKind::Bash, "echo $(date) && echo done").is_ok());
    assert!(validate_dialect(ShellKind::Zsh, "echo $(date)").is_ok());
}

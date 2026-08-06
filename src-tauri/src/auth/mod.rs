//! kxen-auth：订阅凭证探测（官方 CLI 凭证存储 -> 新鲜度比较 -> 导入）。
//! external 凭证文件只读不动（no move/rewrite/permission 变更，symlink 拒绝），
//! 首读需用户批准并记忆该批准（consent.rs）；未批准源探测时跳过。

pub mod consent;
pub mod credential;
pub mod oauth_login;
pub mod probe;
pub mod refresh;
pub mod shared_store;

/// AWS SSO OIDC 共享契约（Kiro 登录/refresh 的端点与动态客户端注册）。
pub(crate) mod aws_sso;

pub use credential::{Credential, CredentialKind};
pub use probe::{ProbeOutcome, ProbeRule, probe_all};

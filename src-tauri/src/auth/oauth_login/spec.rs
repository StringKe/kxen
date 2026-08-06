//! 各家 provider 的 OAuth 端点契约静态表。
//! 常量与各家官方 CLI 的公开实现一致（Claude Code / Codex CLI / qwen-code / grok CLI / kimi CLI /
//! VS Code Copilot / gemini-cli / hermes-agent / Antigravity），多源核实；Anthropic / OpenAI / Google /
//! OpenRouter / Antigravity 走授权码 + loopback 回调 + PKCE，Z.AI（ZCode 契约）走授权码无 PKCE，
//! xAI / Kimi / Qwen / GitHub Copilot / MiniMax 走设备授权流。

/// 换票形态：标准 token pair，或 OpenRouter 的 code 换永久 API key，
/// 或 Z.AI（ZCode）的 code -> broker -> z/login -> 铸 durable API key 三阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExchangeKind {
    TokenPair,
    ApiKey,
    ZaiZcode,
}

/// 授权码流契约。
pub struct CodeSpec {
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    /// OpenRouter 的 PKCE 流无 client_id。
    pub client_id: Option<&'static str>,
    /// Google 桌面应用公开 secret，exchange/refresh 都带。
    pub client_secret: Option<&'static str>,
    /// 空串 = 不带 scope 参数（OpenRouter）。
    pub scopes: &'static str,
    /// 0 = 随机端口（Google/OpenRouter 的 loopback 不校验端口）。
    pub callback_port: u16,
    pub callback_path: &'static str,
    /// OpenRouter：回调 path 需拼一次性 uuid（/oauth/callback/{uuid}）。
    pub callback_path_uuid: bool,
    /// redirect 参数名：标准 redirect_uri；OpenRouter 是 callback_url。
    pub redirect_param: &'static str,
    /// 授权 URL 的厂商私有附加参数。
    pub extra_authorize: &'static [(&'static str, &'static str)],
    /// Anthropic 要求 JSON body；其余为标准 form。
    pub json_body: bool,
    /// Anthropic 非标准点：state 直接复用 PKCE verifier。
    pub state_is_verifier: bool,
    /// Z.AI（ZCode）授权端点不带 PKCE；其余都带 S256 challenge。
    pub pkce: bool,
    /// 是否带 state（OpenRouter 不带，回调也不校验）。
    pub use_state: bool,
    /// 回调不可达时允许手贴授权码（code#state / 完整 URL / 纯 code）。
    pub manual_paste: bool,
    /// OpenAI：account_id 从 access_token JWT 的 auth claim 提取，缺失即登录失败。
    pub account_id_from_jwt: bool,
    pub exchange_kind: ExchangeKind,
}

/// 设备流变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFlavor {
    /// RFC 8628 标准；pkce = Qwen 变体（设备码请求带 challenge，轮询回传 verifier）。
    Rfc8628 { pkce: bool },
    /// MiniMax 非标准：response_type=code + PKCE + state 回显；轮询 grant_type=user_code；
    /// 响应 200 带 status 字段（pending/success/error）；expired_in 双语义（TTL 秒或毫秒时间戳）。
    MiniMax,
    /// AWS SSO OIDC（Kiro）：registerClient 前置拿动态 clientId/clientSecret；三步全 JSON camelCase。
    AwsSso,
}

/// 设备授权流契约。
pub struct DeviceSpec {
    pub device_url: &'static str,
    pub token_url: &'static str,
    pub client_id: &'static str,
    pub scope: Option<&'static str>,
    /// 设备码请求的厂商私有附加字段。
    pub extra_device: &'static [(&'static str, &'static str)],
    pub flavor: DeviceFlavor,
    /// GitHub Copilot 二阶段：OAuth token 换短命 Copilot API JWT。
    pub copilot_exchange: bool,
    /// 轮询/换票请求的附加头（GitHub 要求显式 Accept: application/json）。
    pub extra_headers: &'static [(&'static str, &'static str)],
}

pub enum FlowSpec {
    Code(&'static CodeSpec),
    Device(&'static DeviceSpec),
}

const ANTHROPIC_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://platform.claude.com/v1/oauth/token",
    client_id: Some("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
    client_secret: None,
    scopes: "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
    callback_port: 53692,
    callback_path: "/callback",
    callback_path_uuid: false,
    redirect_param: "redirect_uri",
    extra_authorize: &[("code", "true")],
    json_body: true,
    state_is_verifier: true,
    pkce: true,
    use_state: true,
    manual_paste: true,
    account_id_from_jwt: false,
    exchange_kind: ExchangeKind::TokenPair,
};

const OPENAI_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    client_id: Some("app_EMoamEEZ73f0CkXaXp7hrann"),
    client_secret: None,
    scopes: "openid profile email offline_access",
    callback_port: 1455,
    callback_path: "/auth/callback",
    callback_path_uuid: false,
    redirect_param: "redirect_uri",
    extra_authorize: &[("id_token_add_organizations", "true"), ("codex_cli_simplified_flow", "true"), ("originator", "kxen")],
    json_body: false,
    state_is_verifier: false,
    pkce: true,
    use_state: true,
    manual_paste: false,
    account_id_from_jwt: true,
    exchange_kind: ExchangeKind::TokenPair,
};

/// Google Gemini Code Assist（gemini-cli 公开桌面凭证；loopback 端口任意）。
/// 注意：Google ToS 限制第三方复用该凭证，设置页需向用户明示风险。
/// key 用 google-oauth 与 API-key 版 google 区分（同 qwen / qwen-oauth 模式）。
const GOOGLE_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    client_id: Some("681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com"),
    client_secret: Some("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"),
    scopes: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile",
    callback_port: 0,
    callback_path: "/oauth2callback",
    callback_path_uuid: false,
    redirect_param: "redirect_uri",
    extra_authorize: &[("access_type", "offline"), ("prompt", "consent")],
    json_body: false,
    state_is_verifier: false,
    pkce: true,
    use_state: true,
    manual_paste: false,
    account_id_from_jwt: false,
    exchange_kind: ExchangeKind::TokenPair,
};

/// Google Antigravity（opencode-antigravity-auth / antigravity-auth 多源实证的桌面凭证）。
/// 与 GOOGLE_CODE 同授权端点，但该 client 只保证 51121 回调端口可用（被占时 bind_callback 回退随机端口）。
/// 注意：同 Google ToS 风险，设置页与 google-oauth 共用账号风险警告。
const ANTIGRAVITY_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    client_id: Some("1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"),
    client_secret: Some("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"),
    scopes: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs",
    callback_port: 51121,
    callback_path: "/oauth-callback",
    callback_path_uuid: false,
    redirect_param: "redirect_uri",
    extra_authorize: &[("access_type", "offline"), ("prompt", "consent")],
    json_body: false,
    state_is_verifier: false,
    pkce: true,
    use_state: true,
    manual_paste: false,
    account_id_from_jwt: false,
    exchange_kind: ExchangeKind::TokenPair,
};

/// OpenRouter：PKCE 换永久 API key（无 client_id/state，回调 path 带一次性 uuid）。
const OPENROUTER_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://openrouter.ai/auth",
    token_url: "https://openrouter.ai/api/v1/auth/keys",
    client_id: None,
    client_secret: None,
    scopes: "",
    callback_port: 0,
    callback_path: "/oauth/callback",
    callback_path_uuid: true,
    redirect_param: "callback_url",
    extra_authorize: &[],
    json_body: true,
    state_is_verifier: false,
    pkce: true,
    use_state: false,
    manual_paste: true,
    account_id_from_jwt: false,
    exchange_kind: ExchangeKind::ApiKey,
};

/// 智谱 Z.AI（逆向 ZCode 桌面客户端契约，TriDefender/zcode-api、Yeachan-Heo/gajae-code 等多源核实）。
/// 授权端点不带 PKCE；拿到 code 后走 zcode.z.ai broker -> api.z.ai z/login -> 铸 durable API key
/// 三阶段（见 zai_zcode.rs），最终落 Api 凭证无需刷新。官方未开放第三方登录，契约可能失效。
const ZAI_ZCODE_CODE: CodeSpec = CodeSpec {
    authorize_url: "https://chat.z.ai/api/oauth/authorize",
    token_url: "https://zcode.z.ai/api/v1/oauth/token",
    client_id: Some("client_P8X5CMWmlaRO9gyO-KSqtg"),
    client_secret: None,
    scopes: "",
    callback_port: 0,
    callback_path: "/oauth/callback/zai",
    callback_path_uuid: false,
    redirect_param: "redirect_uri",
    extra_authorize: &[],
    json_body: true,
    state_is_verifier: false,
    pkce: false,
    use_state: true,
    manual_paste: true,
    account_id_from_jwt: false,
    exchange_kind: ExchangeKind::ZaiZcode,
};

const XAI_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://auth.x.ai/oauth2/device/code",
    token_url: "https://auth.x.ai/oauth2/token",
    client_id: "b1a00492-073a-47ea-816f-4c329264a828",
    scope: Some("openid profile email offline_access grok-cli:access api:access"),
    extra_device: &[],
    flavor: DeviceFlavor::Rfc8628 { pkce: false },
    copilot_exchange: false,
    extra_headers: &[],
};

const KIMI_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://auth.kimi.com/api/oauth/device_authorization",
    token_url: "https://auth.kimi.com/api/oauth/token",
    client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
    scope: None,
    extra_device: &[],
    flavor: DeviceFlavor::Rfc8628 { pkce: false },
    copilot_exchange: false,
    extra_headers: &[],
};

const QWEN_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://chat.qwen.ai/api/v1/oauth2/device/code",
    token_url: "https://chat.qwen.ai/api/v1/oauth2/token",
    client_id: "f0304373b74a44d2b584a3fb70ca9e56",
    scope: Some("openid profile email model.completion"),
    extra_device: &[],
    flavor: DeviceFlavor::Rfc8628 { pkce: true },
    copilot_exchange: false,
    extra_headers: &[],
};

const COPILOT_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    client_id: "Iv1.b507a08c87ecfe98",
    scope: Some("read:user"),
    extra_device: &[],
    flavor: DeviceFlavor::Rfc8628 { pkce: false },
    copilot_exchange: true,
    extra_headers: &[("Accept", "application/json")],
};

const MINIMAX_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://api.minimax.io/oauth/code",
    token_url: "https://api.minimax.io/oauth/token",
    client_id: "78257093-7e40-4613-99e0-527b14b39113",
    scope: Some("group_id profile model.completion"),
    extra_device: &[],
    flavor: DeviceFlavor::MiniMax,
    copilot_exchange: false,
    extra_headers: &[("Accept", "application/json")],
};

const MINIMAX_CN_DEVICE: DeviceSpec = DeviceSpec {
    device_url: "https://api.minimaxi.com/oauth/code",
    token_url: "https://api.minimaxi.com/oauth/token",
    client_id: "78257093-7e40-4613-99e0-527b14b39113",
    scope: Some("group_id profile model.completion"),
    extra_device: &[],
    flavor: DeviceFlavor::MiniMax,
    copilot_exchange: false,
    extra_headers: &[("Accept", "application/json")],
};

/// AWS Kiro（Builder ID 设备流）：clientId/clientSecret 由 registerClient 动态签发，
/// client_id 常量槽仅占位不参与请求；register/startUrl 常量见 crate::auth::aws_sso。
const KIRO_DEVICE: DeviceSpec = DeviceSpec {
    device_url: crate::auth::aws_sso::DEVICE_URL,
    token_url: crate::auth::aws_sso::TOKEN_URL,
    client_id: "",
    scope: None,
    extra_device: &[],
    flavor: DeviceFlavor::AwsSso,
    copilot_exchange: false,
    extra_headers: &[],
};

/// 应用内登录支持的 provider；其余走 CLI 导入或 API key。
pub fn spec_for(provider: &str) -> Option<FlowSpec> {
    match provider {
        "anthropic" => Some(FlowSpec::Code(&ANTHROPIC_CODE)),
        "openai" => Some(FlowSpec::Code(&OPENAI_CODE)),
        "google-oauth" => Some(FlowSpec::Code(&GOOGLE_CODE)),
        "google-antigravity" => Some(FlowSpec::Code(&ANTIGRAVITY_CODE)),
        "openrouter" => Some(FlowSpec::Code(&OPENROUTER_CODE)),
        "zhipu-coding" => Some(FlowSpec::Code(&ZAI_ZCODE_CODE)),
        "xai" => Some(FlowSpec::Device(&XAI_DEVICE)),
        "kimi-for-coding" => Some(FlowSpec::Device(&KIMI_DEVICE)),
        "qwen-oauth" => Some(FlowSpec::Device(&QWEN_DEVICE)),
        "github-copilot" => Some(FlowSpec::Device(&COPILOT_DEVICE)),
        "minimax-oauth" => Some(FlowSpec::Device(&MINIMAX_DEVICE)),
        "minimax-cn-oauth" => Some(FlowSpec::Device(&MINIMAX_CN_DEVICE)),
        "kiro" => Some(FlowSpec::Device(&KIRO_DEVICE)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_uses_https_endpoints() {
        for provider in [
            "anthropic",
            "openai",
            "google-oauth",
            "google-antigravity",
            "openrouter",
            "zhipu-coding",
            "xai",
            "kimi-for-coding",
            "qwen-oauth",
            "github-copilot",
            "minimax-oauth",
            "minimax-cn-oauth",
            "kiro",
        ] {
            match spec_for(provider).expect("spec must exist") {
                FlowSpec::Code(spec) => {
                    assert!(spec.authorize_url.starts_with("https://"), "{provider}");
                    assert!(spec.token_url.starts_with("https://"), "{provider}");
                }
                FlowSpec::Device(spec) => {
                    assert!(spec.device_url.starts_with("https://"), "{provider}");
                    assert!(spec.token_url.starts_with("https://"), "{provider}");
                }
            }
        }
        assert!(spec_for("deepseek").is_none());
    }
}

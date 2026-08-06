//! 智谱 Z.AI（ZCode 桌面客户端契约，逆向，无官方承诺）的三阶段换票。契约来源交叉核实：
//! TriDefender/zcode-api src/auth/oauth.ts、Yeachan-Heo/gajae-code glm-zcode.ts、
//! smartlizi/zcode-account-switcher src/oauthCli.js、jlcodes99/cockpit-tools zcode_oauth.rs。
//! 1. broker：POST zcode.z.ai/api/v1/oauth/token {provider:"zai", code, redirect_uri, state}
//!    -> {code:0, data:{token:<ZCode JWT>, zai:{access_token:<上游 Z.AI token>}, user}}
//! 2. z/login：POST api.z.ai/api/auth/z/login {token:<上游 token>} -> {data:{access_token:<业务 token>}}
//! 3. 铸 key：业务 token 作 Bearer，getCustomerInfo 取默认 org/project，复用或创建名为 zcode-api-key
//!    的 API key，再 GET api_keys/copy/{id} 取 secretKey，拼 "{id}.{secretKey}"，落 CredentialKind::Api。

use crate::auth::credential::CredentialKind;
use serde_json::{Value, json};

const ZAI_LOGIN_URL: &str = "https://api.z.ai/api/auth/z/login";
const ZAI_API_BASE: &str = "https://api.z.ai";
/// ZCode 官方客户端自动铸 key 时用的固定名字（host bundle 常量）。
const API_KEY_NAME: &str = "zcode-api-key";

/// 三阶段端点；生产取常量，测试指向 mock。
struct Endpoints {
    broker: String,
    z_login: String,
    api_base: String,
}

/// code_flow 的 ZaiZcode 分支入口：broker_url 即 CodeSpec.token_url。
pub(super) async fn exchange(
    client: &reqwest::Client,
    broker_url: &str,
    code: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<CredentialKind, String> {
    let endpoints = Endpoints { broker: broker_url.to_string(), z_login: ZAI_LOGIN_URL.to_string(), api_base: ZAI_API_BASE.to_string() };
    run(client, &endpoints, code, redirect_uri, state).await
}

async fn run(
    client: &reqwest::Client,
    endpoints: &Endpoints,
    code: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<CredentialKind, String> {
    let broker = post_json(
        client,
        &endpoints.broker,
        &json!({ "provider": "zai", "code": code, "redirect_uri": redirect_uri, "state": state }),
        None,
        "Z.AI broker",
    )
    .await?;
    if let Some(error) = envelope_error(&broker) {
        return Err(format!("Z.AI broker 换票失败：{error}"));
    }
    let upstream = broker
        .pointer("/data/zai/access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("Z.AI broker 响应缺少 data.zai.access_token")?;

    let login = post_json(client, &endpoints.z_login, &json!({ "token": upstream }), None, "Z.AI z/login").await?;
    if let Some(error) = envelope_error(&login) {
        return Err(format!("Z.AI z/login 失败：{error}"));
    }
    let business = login
        .pointer("/data/access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("Z.AI z/login 响应缺少 data.access_token")?;

    provision_api_key(client, endpoints, business).await
}

/// 阶段 3：业务 token 下复用或创建 durable API key，返回 "{id}.{secretKey}"。
async fn provision_api_key(client: &reqwest::Client, endpoints: &Endpoints, business: &str) -> Result<CredentialKind, String> {
    let info =
        get_json(client, &format!("{}/api/biz/customer/getCustomerInfo", endpoints.api_base), business, "Z.AI getCustomerInfo").await?;
    let (organization, project) = pick_default_org_project(&info)?;
    let keys_url = format!("{}/api/biz/v1/organization/{organization}/projects/{project}/api_keys", endpoints.api_base);

    let list = get_json(client, &keys_url, business, "Z.AI api_keys.list").await?;
    let existing = list
        .get("data")
        .and_then(Value::as_array)
        .and_then(|entries| entries.iter().find(|entry| entry.get("name").and_then(Value::as_str) == Some(API_KEY_NAME)))
        .and_then(api_key_id);
    let id = match existing {
        Some(id) => id,
        None => {
            let created = post_json(client, &keys_url, &json!({ "name": API_KEY_NAME }), Some(business), "Z.AI api_keys.create").await?;
            let entry = created.get("data").filter(|data| data.is_object()).unwrap_or(&created);
            api_key_id(entry).ok_or("Z.AI api_keys.create 响应缺少 apiKey id")?
        }
    };

    let copy = get_json(client, &format!("{keys_url}/copy/{id}"), business, "Z.AI api_keys.copy").await?;
    let data = copy.get("data").filter(|data| data.is_object()).unwrap_or(&copy);
    let secret = data
        .get("secretKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("Z.AI api_keys.copy 响应缺少 secretKey")?;
    Ok(CredentialKind::Api { key: format!("{id}.{secret}"), region: None })
}

/// z.ai 系信封：code 缺失/null 视为成功；数字 0/200、字符串 "0"/"200" 成功；
/// success=false 强制失败。失败原因取 msg。
fn envelope_error(value: &Value) -> Option<String> {
    let code_ok = match value.get("code") {
        None | Some(Value::Null) => true,
        Some(Value::Number(code)) => code.as_i64().is_some_and(|code| code == 0 || code == 200),
        Some(Value::String(code)) => matches!(code.trim(), "0" | "200"),
        _ => false,
    };
    if code_ok && value.get("success").and_then(Value::as_bool) != Some(false) {
        return None;
    }
    Some(value.get("msg").and_then(Value::as_str).unwrap_or("未知错误").to_string())
}

/// getCustomerInfo 取默认（缺省取首个）organization 与 project。
fn pick_default_org_project(payload: &Value) -> Result<(String, String), String> {
    let root = payload.get("data").filter(|data| data.is_object()).unwrap_or(payload);
    let organizations = root.get("organizations").and_then(Value::as_array).ok_or("Z.AI getCustomerInfo 响应缺少 organizations")?;
    let organization = organizations
        .iter()
        .find(|org| org.get("isDefault").and_then(Value::as_bool) == Some(true))
        .or_else(|| organizations.first())
        .ok_or("Z.AI getCustomerInfo organizations 为空")?;
    let organization_id = organization
        .get("organizationId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("Z.AI getCustomerInfo 缺少 organizationId")?;
    let projects = organization.get("projects").and_then(Value::as_array).ok_or("Z.AI getCustomerInfo 缺少 projects")?;
    let project = projects
        .iter()
        .find(|proj| proj.get("isDefault").and_then(Value::as_bool) == Some(true))
        .or_else(|| projects.first())
        .ok_or("Z.AI getCustomerInfo projects 为空")?;
    let project_id =
        project.get("projectId").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or("Z.AI getCustomerInfo 缺少 projectId")?;
    Ok((organization_id.to_string(), project_id.to_string()))
}

/// api_keys 条目的 id 字段：官方响应为 apiKey，兼容 id 别名。
fn api_key_id(entry: &Value) -> Option<String> {
    entry
        .get("apiKey")
        .and_then(Value::as_str)
        .or_else(|| entry.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(String::from)
}

async fn post_json(client: &reqwest::Client, url: &str, body: &Value, bearer: Option<&str>, label: &str) -> Result<Value, String> {
    let mut request = client.post(url).json(body);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    read_json(request.send().await.map_err(|error| format!("{label} 请求失败: {error}"))?, label).await
}

async fn get_json(client: &reqwest::Client, url: &str, bearer: &str, label: &str) -> Result<Value, String> {
    let response = client.get(url).bearer_auth(bearer).send().await.map_err(|error| format!("{label} 请求失败: {error}"))?;
    read_json(response, label).await
}

async fn read_json(response: reqwest::Response, label: &str) -> Result<Value, String> {
    let status = response.status();
    let value = crate::net_response::json::<Value>(response, crate::net_response::JSON_BODY_LIMIT, label).await?;
    if !status.is_success() {
        let detail = value.get("msg").or_else(|| value.get("error")).or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("{label} 失败：http {status} {detail}"));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, String, String, String)>>>;

    /// 顺序应答的 mock server：记录 (method, path, authorization, body)，按 path 路由响应。
    fn mock_server(handler: impl Fn(&str, &str) -> (u16, String) + Send + 'static) -> (String, Seen) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let seen_in = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let (method, path, authorization, body) = read_request(&mut stream);
                let (status, response) = handler(&method, &path);
                crate::core::shared::lock(&seen_in).push((method, path, authorization, body));
                let reply = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
                    response.len()
                );
                if stream.write_all(reply.as_bytes()).is_err() {
                    break;
                }
            }
        });
        (format!("http://{address}"), seen)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> (String, String, String, String) {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break buffer.len();
            }
            buffer.extend_from_slice(&chunk[..n]);
        };
        let header = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
        let header_value = |name: &str| {
            header.lines().find_map(|line| line.to_ascii_lowercase().starts_with(name).then(|| line[name.len()..].trim().to_string()))
        };
        let content_length = header_value("content-length:").and_then(|value| value.parse::<usize>().ok()).unwrap_or(0);
        while buffer.len() < header_end + content_length {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buffer[header_end..]).into_owned();
        let mut parts = header.split_whitespace();
        (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("").to_string(),
            header_value("authorization:").unwrap_or_default(),
            body,
        )
    }

    fn endpoints(base: &str) -> Endpoints {
        Endpoints { broker: format!("{base}/oauth/token"), z_login: format!("{base}/api/auth/z/login"), api_base: base.to_string() }
    }

    const CUSTOMER_INFO: &str =
        r#"{"data":{"organizations":[{"organizationId":"org-1","isDefault":true,"projects":[{"projectId":"proj-1","isDefault":true}]}]}}"#;

    #[test]
    fn spec_registered_as_zai_zcode_exchange() {
        let Some(super::super::spec::FlowSpec::Code(spec)) = super::super::spec::spec_for("zhipu-coding") else {
            panic!("zhipu-coding spec missing")
        };
        assert_eq!(spec.exchange_kind, super::super::spec::ExchangeKind::ZaiZcode);
        assert!(!spec.pkce);
        assert!(spec.use_state && spec.manual_paste);
        assert_eq!(spec.token_url, "https://zcode.z.ai/api/v1/oauth/token");
    }

    #[test]
    fn envelope_accepts_documented_success_shapes() {
        for ok in [json!({"code": 0}), json!({"code": 200}), json!({"code": "0"}), json!({"data": {}}), json!({"code": null})] {
            assert!(envelope_error(&ok).is_none(), "{ok}");
        }
        assert_eq!(
            envelope_error(&json!({"code": 401, "msg": "authorization code expired"})).as_deref(),
            Some("authorization code expired")
        );
        assert_eq!(envelope_error(&json!({"code": 0, "success": false, "msg": "oauth required"})).as_deref(), Some("oauth required"));
    }

    #[test]
    fn pick_default_org_project_prefers_default_and_falls_back_to_first() {
        let (org, project) = pick_default_org_project(&json!({"data":{"organizations":[
            {"organizationId":"org-a","projects":[{"projectId":"proj-a"}]},
            {"organizationId":"org-b","isDefault":true,"projects":[{"projectId":"p1"},{"projectId":"p2","isDefault":true}]}
        ]}}))
        .unwrap();
        assert_eq!((org.as_str(), project.as_str()), ("org-b", "p2"));
        assert!(pick_default_org_project(&json!({"data": {"organizations": []}})).is_err());
        assert!(pick_default_org_project(&json!({"data": {}})).is_err());
    }

    #[tokio::test]
    async fn three_stage_flow_reuses_existing_api_key() {
        let (base, seen) = mock_server(|method, path| match (method, path) {
            ("POST", "/oauth/token") => (
                200,
                json!({"code":0,"data":{"token":"zcode-jwt","zai":{"access_token":"upstream-token"},"user":{"user_id":"u1"}}}).to_string(),
            ),
            ("POST", "/api/auth/z/login") => (200, json!({"code":0,"data":{"access_token":"business-token"}}).to_string()),
            ("GET", "/api/biz/customer/getCustomerInfo") => (200, CUSTOMER_INFO.to_string()),
            ("GET", "/api/biz/v1/organization/org-1/projects/proj-1/api_keys") => {
                (200, json!({"data":[{"name":"other","apiKey":"k0"},{"name":"zcode-api-key","apiKey":"key-id"}]}).to_string())
            }
            ("GET", "/api/biz/v1/organization/org-1/projects/proj-1/api_keys/copy/key-id") => {
                (200, json!({"data":{"secretKey":"secret"}}).to_string())
            }
            _ => (404, json!({"msg":"unexpected"}).to_string()),
        });
        let credential = run(&reqwest::Client::new(), &endpoints(&base), "the-code", "http://localhost:1/cb", "the-state").await.unwrap();
        assert!(matches!(credential, CredentialKind::Api { ref key, region: None } if key == "key-id.secret"));
        let seen = crate::core::shared::lock(&seen);
        assert_eq!(seen.len(), 5, "{seen:?}");
        let broker_body: Value = serde_json::from_str(&seen[0].3).unwrap();
        assert_eq!(broker_body, json!({"provider":"zai","code":"the-code","redirect_uri":"http://localhost:1/cb","state":"the-state"}));
        let login_body: Value = serde_json::from_str(&seen[1].3).unwrap();
        assert_eq!(login_body, json!({"token":"upstream-token"}));
        assert!(seen[2..].iter().all(|(_, _, auth, _)| auth == "Bearer business-token"), "{seen:?}");
    }

    #[tokio::test]
    async fn three_stage_flow_creates_key_when_missing() {
        let (base, seen) = mock_server(|method, path| match (method, path) {
            ("POST", "/oauth/token") => (200, json!({"data":{"token":"jwt","zai":{"access_token":"up"}}}).to_string()),
            ("POST", "/api/auth/z/login") => (200, json!({"data":{"access_token":"biz"}}).to_string()),
            ("GET", "/api/biz/customer/getCustomerInfo") => (200, CUSTOMER_INFO.to_string()),
            ("GET", "/api/biz/v1/organization/org-1/projects/proj-1/api_keys") => (200, json!({"data":[]}).to_string()),
            ("POST", "/api/biz/v1/organization/org-1/projects/proj-1/api_keys") => (200, json!({"data":{"apiKey":"new-id"}}).to_string()),
            ("GET", "/api/biz/v1/organization/org-1/projects/proj-1/api_keys/copy/new-id") => {
                (200, json!({"data":{"secretKey":"new-secret"}}).to_string())
            }
            _ => (404, json!({"msg":"unexpected"}).to_string()),
        });
        let credential = run(&reqwest::Client::new(), &endpoints(&base), "c", "r", "s").await.unwrap();
        assert!(matches!(credential, CredentialKind::Api { ref key, .. } if key == "new-id.new-secret"));
        let seen = crate::core::shared::lock(&seen);
        let create_body: Value = serde_json::from_str(&seen[4].3).unwrap();
        assert_eq!(create_body, json!({"name":"zcode-api-key"}));
    }

    #[tokio::test]
    async fn broker_failures_surface_loudly() {
        let (expired, _) = mock_server(|_, _| (200, json!({"code":401,"msg":"authorization code expired"}).to_string()));
        let error = run(&reqwest::Client::new(), &endpoints(&expired), "c", "r", "s").await.unwrap_err();
        assert!(error.contains("authorization code expired"), "{error}");
        let (missing, _) = mock_server(|_, _| (200, json!({"code":0,"data":{"token":"jwt"}}).to_string()));
        let error = run(&reqwest::Client::new(), &endpoints(&missing), "c", "r", "s").await.unwrap_err();
        assert!(error.contains("data.zai.access_token"), "{error}");
    }
}

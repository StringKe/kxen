//! OAuth loopback callback parsing and admission.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt};

const REQUEST_LINE_LIMIT: usize = 4 * 1024;
const HEADER_LINE_LIMIT: usize = 8 * 1024;
const HEADER_TOTAL_LIMIT: usize = 32 * 1024;
const CONNECTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, Default)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// 绑回调端口：配置端口被占回退 :0 随机（固定端口只是便利，不该让授权流起不来）。
pub async fn bind_callback(port: Option<u16>) -> Result<(tokio::net::TcpListener, u16), String> {
    if let Some(port) = port {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return Ok((listener, port)),
            Err(error) => tracing::warn!(port, %error, "oauth 回调端口被占，回退随机端口"),
        }
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.map_err(|error| format!("oauth 回调端口绑定失败: {error}"))?;
    let port = listener.local_addr().map_err(|error| error.to_string())?.port();
    Ok((listener, port))
}

/// 只有 path 匹配才消费回调；expected_state 为 Some 时还要求 state 一致（OpenRouter 无 state 传 None）。
/// 无效或慢速连接被逐个拒绝，但不会夺走真实浏览器回调的等待机会。
pub async fn wait_callback(
    listener: &tokio::net::TcpListener,
    expected_path: &str,
    expected_state: Option<&str>,
    timeout: std::time::Duration,
) -> Result<CallbackParams, String> {
    let work = async {
        loop {
            let (socket, _) = listener.accept().await.map_err(|error| error.to_string())?;
            let mut reader = tokio::io::BufReader::new(socket);
            let target = tokio::time::timeout(CONNECTION_TIMEOUT, read_target(&mut reader)).await;
            let mut socket = reader.into_inner();
            let target = match target {
                Ok(Ok(Some(target))) => target,
                Ok(Ok(None)) => continue,
                Ok(Err(status)) => {
                    respond_empty(&mut socket, status).await;
                    continue;
                }
                Err(_) => {
                    respond_empty(&mut socket, "408 Request Timeout").await;
                    continue;
                }
            };
            let Ok(parsed) = reqwest::Url::parse(&format!("http://127.0.0.1{target}")) else {
                respond_empty(&mut socket, "400 Bad Request").await;
                continue;
            };
            if parsed.path() != expected_path {
                respond_empty(&mut socket, "404 Not Found").await;
                continue;
            }
            let out = parse_params(&parsed);
            let state_ok = match expected_state {
                Some(expected) => out.state.as_deref() == Some(expected),
                None => true,
            };
            if !state_ok || (out.code.is_none() && out.error.is_none()) {
                respond_empty(&mut socket, "400 Bad Request").await;
                continue;
            }
            let html = "<html><body><h3>kxen MCP 认证完成，可以关闭本页面</h3></body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = socket.write_all(response.as_bytes()).await;
            return Ok(out);
        }
    };
    tokio::time::timeout(timeout, work).await.unwrap_or_else(|_| Err("oauth 等待回调超时".into()))
}

async fn read_target(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<String>, &'static str> {
    let Some(request_line) = read_bounded_line(reader, REQUEST_LINE_LIMIT).await? else { return Ok(None) };
    let request_line = std::str::from_utf8(&request_line).map_err(|_| "400 Bad Request")?;
    let mut parts = request_line.split_whitespace();
    if parts.next() != Some("GET") {
        return Err("405 Method Not Allowed");
    }
    let target = parts.next().ok_or("400 Bad Request")?.to_string();
    if parts.next().is_none() || parts.next().is_some() {
        return Err("400 Bad Request");
    }

    let mut total = request_line.len();
    loop {
        let Some(line) = read_bounded_line(reader, HEADER_LINE_LIMIT).await? else { return Err("400 Bad Request") };
        total = total.checked_add(line.len()).ok_or("431 Request Header Fields Too Large")?;
        if total > HEADER_TOTAL_LIMIT {
            return Err("431 Request Header Fields Too Large");
        }
        if line == b"\r\n" || line == b"\n" {
            return Ok(Some(target));
        }
    }
}

async fn read_bounded_line(reader: &mut (impl AsyncBufRead + Unpin), limit: usize) -> Result<Option<Vec<u8>>, &'static str> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.map_err(|_| "400 Bad Request")?;
        if available.is_empty() {
            return if line.is_empty() { Ok(None) } else { Err("400 Bad Request") };
        }
        let take = available.iter().position(|byte| *byte == b'\n').map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > limit {
            return Err("431 Request Header Fields Too Large");
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(Some(line));
        }
    }
}

fn parse_params(url: &reqwest::Url) -> CallbackParams {
    let mut out = CallbackParams::default();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => out.code = Some(value.into_owned()),
            "state" => out.state = Some(value.into_owned()),
            "error" => out.error = Some(value.into_owned()),
            "error_description" => out.error_description = Some(value.into_owned()),
            _ => {}
        }
    }
    out
}

async fn respond_empty(socket: &mut tokio::net::TcpStream, status: &str) {
    let response = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    let _ = socket.write_all(response.as_bytes()).await;
}

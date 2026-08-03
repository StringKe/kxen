use std::collections::HashSet;

pub(super) const MAX_HEADER_BYTES: usize = 32 * 1024;
pub(super) const MAX_HEADER_COUNT: usize = 100;
pub(super) const MAX_LINE_BYTES: usize = 8 * 1024;
pub(super) const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Target {
    pub(super) host: String,
    pub(super) port: u16,
}

#[derive(Debug)]
pub(super) enum Request {
    Connect(Target),
    Forward { target: Target, bytes: Vec<u8>, body_len: u64, websocket: bool },
}

pub(super) fn parse(header: &[u8]) -> Result<Request, String> {
    if header.len() > MAX_HEADER_BYTES || !header.ends_with(b"\r\n\r\n") {
        return Err("proxy request headers exceed limit or are incomplete".into());
    }
    let text = std::str::from_utf8(header).map_err(|_| "proxy request headers are not UTF-8")?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let first = lines.next().ok_or("proxy request line missing")?;
    if first.len() > MAX_LINE_BYTES {
        return Err("proxy request line exceeds limit".into());
    }
    let mut parts = first.split(' ');
    let method = parts.next().ok_or("proxy method missing")?;
    let uri = parts.next().ok_or("proxy request target missing")?;
    let version = parts.next().ok_or("proxy HTTP version missing")?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") || !valid_token(method) {
        return Err("invalid proxy request line".into());
    }
    let headers = parse_headers(lines)?;
    let body_len = body_length(&headers)?;
    if method.eq_ignore_ascii_case("CONNECT") {
        if body_len != 0 {
            return Err("CONNECT request bodies are not allowed".into());
        }
        let target = parse_authority(uri, true)?;
        ensure_host_matches(&headers, &target)?;
        return Ok(Request::Connect(target));
    }
    let url = reqwest::Url::parse(uri).map_err(|error| format!("proxy requires an absolute HTTP URL: {error}"))?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("proxy forward requests require an http URL without credentials or fragment".into());
    }
    let host = normalized_host(&url)?;
    let port = url.port_or_known_default().ok_or("proxy URL has no port")?;
    let target = Target { host, port };
    ensure_host_matches(&headers, &target)?;
    let path = if let Some(query) = url.query() { format!("{}?{query}", url.path()) } else { url.path().to_string() };
    let (bytes, websocket) = rewrite_forward(method, &path, version, &headers, &target)?;
    if websocket && (!method.eq_ignore_ascii_case("GET") || body_len != 0) {
        return Err("WebSocket upgrade must be a GET request without a body".into());
    }
    Ok(Request::Forward { target, bytes, body_len, websocket })
}

fn parse_headers<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Vec<(&'a str, &'a str)>, String> {
    let mut headers = Vec::new();
    let mut names = HashSet::new();
    for line in lines {
        if line.len() > MAX_LINE_BYTES || line.starts_with([' ', '\t']) {
            return Err("invalid or oversized proxy header line".into());
        }
        let (name, value) = line.split_once(':').ok_or("invalid proxy header")?;
        if !valid_token(name) || value.bytes().any(|byte| byte < 0x20 && byte != b'\t') {
            return Err("invalid proxy header name or value".into());
        }
        let lower = name.to_ascii_lowercase();
        if !names.insert(lower.clone()) && matches!(lower.as_str(), "host" | "content-length" | "transfer-encoding") {
            return Err(format!("duplicate security-sensitive proxy header: {name}"));
        }
        headers.push((name, value.trim()));
        if headers.len() > MAX_HEADER_COUNT {
            return Err(format!("proxy request has more than {MAX_HEADER_COUNT} headers"));
        }
    }
    Ok(headers)
}

fn parse_authority(authority: &str, require_port: bool) -> Result<Target, String> {
    if authority.contains(['/', '?', '#', '@']) {
        return Err("invalid CONNECT authority".into());
    }
    let url = reqwest::Url::parse(&format!("http://{authority}/")).map_err(|error| format!("invalid CONNECT authority: {error}"))?;
    let host = normalized_host(&url)?;
    let explicit_port = if authority.starts_with('[') {
        authority.find(']').is_some_and(|end| authority.as_bytes().get(end + 1) == Some(&b':'))
    } else {
        authority.rsplit_once(':').is_some()
    };
    if require_port && !explicit_port {
        return Err("CONNECT authority must include a port".into());
    }
    let port = url.port_or_known_default().ok_or("CONNECT authority has no port")?;
    Ok(Target { host, port })
}

fn ensure_host_matches(headers: &[(&str, &str)], target: &Target) -> Result<(), String> {
    let host = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("host")).ok_or("proxy Host header missing")?.1;
    let parsed = parse_authority(host, false)?;
    if !parsed.host.eq_ignore_ascii_case(&target.host) || parsed.port != target.port {
        return Err("proxy Host header does not match request target".into());
    }
    Ok(())
}

fn rewrite_forward(method: &str, path: &str, version: &str, headers: &[(&str, &str)], target: &Target) -> Result<(Vec<u8>, bool), String> {
    let upgrade = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("upgrade")).map(|(_, value)| *value);
    if upgrade.is_some_and(|value| !value.eq_ignore_ascii_case("websocket")) {
        return Err("only WebSocket HTTP upgrades are allowed through browser proxy".into());
    }
    if upgrade.is_some()
        && !headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("connection"))
            .any(|(_, value)| value.split(',').any(|token| token.trim().eq_ignore_ascii_case("upgrade")))
    {
        return Err("WebSocket request is missing Connection: Upgrade".into());
    }
    let mut out = format!("{method} {path} {version}\r\n").into_bytes();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("host")
            || name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
        {
            continue;
        }
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    let host = if target.host.contains(':') { format!("[{}]", target.host) } else { target.host.clone() };
    let connection = if upgrade.is_some() { "Upgrade" } else { "close" };
    out.extend_from_slice(format!("Host: {host}:{}\r\nConnection: {connection}\r\n\r\n", target.port).as_bytes());
    if out.len() > MAX_HEADER_BYTES {
        return Err("rewritten proxy headers exceed limit".into());
    }
    Ok((out, upgrade.is_some()))
}

fn normalized_host(url: &reqwest::Url) -> Result<String, String> {
    let host = url.host_str().ok_or("proxy URL has no host")?;
    Ok(host.strip_prefix('[').and_then(|value| value.strip_suffix(']')).unwrap_or(host).to_string())
}

fn body_length(headers: &[(&str, &str)]) -> Result<u64, String> {
    let length = match headers.iter().find(|(name, _)| name.eq_ignore_ascii_case("content-length")).map(|(_, value)| *value) {
        Some(value) => value.parse::<u64>().map_err(|_| "invalid proxy Content-Length")?,
        None => 0,
    };
    if length > MAX_BODY_BYTES {
        return Err(format!("proxy request body exceeds {MAX_BODY_BYTES} byte limit"));
    }
    if headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding")) {
        // Blindly tunneling chunked bodies would let a second absolute-form request bypass per-request
        // target validation. Chrome's normal fetch/upload path emits Content-Length; streaming request
        // bodies fail closed instead of weakening the one-target-per-connection invariant.
        return Err("browser proxy requires Content-Length for request bodies".into());
    }
    Ok(length)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
        })
}

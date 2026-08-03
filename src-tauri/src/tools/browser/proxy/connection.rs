use super::request::{self, Request};
use super::{Resolver, connect_target};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const IO_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 64;
const MAX_DIRECTION_BYTES: u64 = request::MAX_BODY_BYTES;

pub(super) async fn serve(
    listener: TcpListener,
    mut shutdown: tokio::sync::oneshot::Receiver<()>,
    resolver: Arc<dyn Resolver>,
    allow_loopback: bool,
) {
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => match accepted {
                Ok((mut client, _)) => {
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        connections.spawn(async move {
                            let _ = respond(&mut client, "503 Service Unavailable", "browser proxy connection limit reached").await;
                        });
                        continue;
                    };
                    let resolver = Arc::clone(&resolver);
                    connections.spawn(async move {
                        let _permit = permit;
                        if let Err(error) = handle(client, resolver.as_ref(), allow_loopback).await {
                            tracing::debug!("browser proxy connection closed: {error}");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!("browser proxy listener failed closed: {error}");
                    break;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn handle(mut client: TcpStream, resolver: &dyn Resolver, allow_loopback: bool) -> Result<(), String> {
    client.set_nodelay(true).map_err(|error| format!("failed to configure proxy client: {error}"))?;
    let (header, remainder) = match read_header(&mut client, "request").await {
        Ok(value) => value,
        Err(error) => {
            let status = if error.contains("limit") { "431 Request Header Fields Too Large" } else { "400 Bad Request" };
            let _ = respond(&mut client, status, &error).await;
            return Err(error);
        }
    };
    let request = match request::parse(&header) {
        Ok(value) => value,
        Err(error) => {
            let status = if error.contains("body exceeds") {
                "413 Content Too Large"
            } else if error.contains("limit") || error.contains("more than") {
                "431 Request Header Fields Too Large"
            } else {
                "400 Bad Request"
            };
            let _ = respond(&mut client, status, &error).await;
            return Err(error);
        }
    };
    let target = match &request {
        Request::Connect(target) | Request::Forward { target, .. } => target,
    };
    if matches!(&request, Request::Connect(_) if !remainder.is_empty())
        || matches!(&request, Request::Forward { body_len, .. } if remainder.len() as u64 > *body_len)
    {
        let error = "proxy request contains bytes beyond its declared body".to_string();
        let _ = respond(&mut client, "400 Bad Request", &error).await;
        return Err(error);
    }
    let upstream = match connect_target(resolver, target, allow_loopback).await {
        Ok(value) => value,
        Err(error) => {
            let status = if error.contains("blocked") || error.contains("dns resolve") { "403 Forbidden" } else { "502 Bad Gateway" };
            let _ = respond(&mut client, status, &error).await;
            return Err(error);
        }
    };
    upstream.set_nodelay(true).map_err(|error| format!("failed to configure proxy upstream: {error}"))?;
    match request {
        Request::Connect(_) => {
            write_all_timeout(&mut client, b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
            tunnel(client, upstream).await
        }
        Request::Forward { bytes, body_len, websocket, .. } => {
            if websocket {
                forward_websocket(client, upstream, &bytes).await
            } else {
                forward_http(client, upstream, &bytes, &remainder, body_len).await
            }
        }
    }
}

async fn read_header(stream: &mut TcpStream, label: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    tokio::time::timeout(HEADER_TIMEOUT, async {
        let mut data = Vec::with_capacity(4096);
        let mut chunk = [0_u8; 2048];
        loop {
            let count = stream.read(&mut chunk).await.map_err(|error| format!("failed to read proxy request: {error}"))?;
            if count == 0 {
                return Err(format!("proxy {label} ended before headers completed"));
            }
            data.extend_from_slice(&chunk[..count]);
            if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n").map(|position| position + 4) {
                if end > request::MAX_HEADER_BYTES {
                    return Err(format!("proxy {label} header limit is {} bytes", request::MAX_HEADER_BYTES));
                }
                let remainder = data.split_off(end);
                return Ok((data, remainder));
            }
            if data.len() > request::MAX_HEADER_BYTES {
                return Err(format!("proxy request header limit is {} bytes", request::MAX_HEADER_BYTES));
            }
        }
    })
    .await
    .map_err(|_| format!("proxy {label} header timed out"))?
}

async fn tunnel(client: TcpStream, upstream: TcpStream) -> Result<(), String> {
    tunnel_with_initial(client, upstream, 0, 0).await
}

async fn tunnel_with_initial(client: TcpStream, upstream: TcpStream, initial_outbound: u64, initial_inbound: u64) -> Result<(), String> {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut upstream_read, mut upstream_write) = upstream.into_split();
    let outbound = pump(&mut client_read, &mut upstream_write, "request", initial_outbound);
    let inbound = pump(&mut upstream_read, &mut client_write, "response", initial_inbound);
    tokio::select! {
        result = outbound => result,
        result = inbound => result,
    }
}

async fn forward_http(mut client: TcpStream, mut upstream: TcpStream, header: &[u8], prefix: &[u8], body_len: u64) -> Result<(), String> {
    write_all_timeout(&mut upstream, header).await?;
    write_all_timeout(&mut upstream, prefix).await?;
    let remaining = body_len - prefix.len() as u64;
    let (mut client_read, mut client_write) = client.split();
    let (mut upstream_read, mut upstream_write) = upstream.split();
    let outbound = async {
        pump_exact(&mut client_read, &mut upstream_write, remaining).await?;
        upstream_write.shutdown().await.map_err(|error| format!("proxy request shutdown failed: {error}"))
    };
    let inbound = pump(&mut upstream_read, &mut client_write, "response", 0);
    tokio::try_join!(outbound, inbound)?;
    Ok(())
}

async fn forward_websocket(mut client: TcpStream, mut upstream: TcpStream, header: &[u8]) -> Result<(), String> {
    write_all_timeout(&mut upstream, header).await?;
    let (response, prefix) = read_header(&mut upstream, "WebSocket response").await?;
    if !websocket_accepted(&response)? {
        upstream.shutdown().await.map_err(|error| format!("proxy WebSocket rejection shutdown failed: {error}"))?;
        write_all_timeout(&mut client, &response).await?;
        write_all_timeout(&mut client, &prefix).await?;
        return pump(&mut upstream, &mut client, "response", (response.len() + prefix.len()) as u64).await;
    }
    write_all_timeout(&mut client, &response).await?;
    write_all_timeout(&mut client, &prefix).await?;
    tunnel_with_initial(client, upstream, header.len() as u64, (response.len() + prefix.len()) as u64).await
}

fn websocket_accepted(header: &[u8]) -> Result<bool, String> {
    let text = std::str::from_utf8(header).map_err(|_| "proxy WebSocket response headers are not UTF-8")?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or("proxy WebSocket response status missing")?;
    let mut status_parts = status.split_ascii_whitespace();
    let switching =
        matches!(status_parts.next(), Some("HTTP/1.0" | "HTTP/1.1")) && status_parts.next() == Some("101") && status_parts.next().is_some();
    let mut upgrade = false;
    let mut connection = false;
    for (index, line) in lines.take_while(|line| !line.is_empty()).enumerate() {
        if index >= request::MAX_HEADER_COUNT || line.len() > request::MAX_LINE_BYTES {
            return Err("proxy WebSocket response headers exceed limit".into());
        }
        let (name, value) = line.split_once(':').ok_or("invalid proxy WebSocket response header")?;
        if name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket") {
            upgrade = true;
        }
        if name.eq_ignore_ascii_case("connection") && value.split(',').any(|token| token.trim().eq_ignore_ascii_case("upgrade")) {
            connection = true;
        }
    }
    Ok(switching && upgrade && connection)
}

async fn pump_exact<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(reader: &mut R, writer: &mut W, mut remaining: u64) -> Result<(), String> {
    let mut buffer = [0_u8; 16 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).expect("read limit fits usize");
        let count = tokio::time::timeout(IO_IDLE_TIMEOUT, reader.read(&mut buffer[..limit]))
            .await
            .map_err(|_| "proxy request idle timeout".to_string())?
            .map_err(|error| format!("proxy request read failed: {error}"))?;
        if count == 0 {
            return Err("proxy request ended before Content-Length bytes arrived".into());
        }
        write_all_timeout(writer, &buffer[..count]).await?;
        remaining -= count as u64;
    }
    Ok(())
}

async fn pump<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    label: &str,
    initial: u64,
) -> Result<(), String> {
    let mut total = initial;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = tokio::time::timeout(IO_IDLE_TIMEOUT, reader.read(&mut buffer))
            .await
            .map_err(|_| format!("proxy {label} idle timeout"))?
            .map_err(|error| format!("proxy {label} read failed: {error}"))?;
        if count == 0 {
            writer.shutdown().await.map_err(|error| format!("proxy {label} shutdown failed: {error}"))?;
            return Ok(());
        }
        total = total.saturating_add(count as u64);
        if total > MAX_DIRECTION_BYTES {
            return Err(format!("proxy {label} exceeds {MAX_DIRECTION_BYTES} byte limit"));
        }
        write_all_timeout(writer, &buffer[..count]).await?;
    }
}

async fn write_all_timeout(writer: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<(), String> {
    tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(bytes))
        .await
        .map_err(|_| "proxy write timed out".to_string())?
        .map_err(|error| format!("proxy write failed: {error}"))
}

async fn respond(stream: &mut TcpStream, status: &str, message: &str) -> Result<(), String> {
    let safe = message.replace(['\r', '\n'], " ");
    let body = safe.as_bytes();
    let response =
        format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{safe}", body.len());
    write_all_timeout(stream, response.as_bytes()).await
}

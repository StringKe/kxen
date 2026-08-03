//! Bounded decoding for responses controlled by remote services.

use futures::StreamExt;
use serde::de::DeserializeOwned;

pub(crate) const ERROR_BODY_LIMIT: usize = 64 * 1024;
pub(crate) const JSON_BODY_LIMIT: usize = 1024 * 1024;
pub(crate) const CATALOG_BODY_LIMIT: usize = 8 * 1024 * 1024;

pub(crate) async fn bytes(response: reqwest::Response, limit: usize, label: &str) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(format!("{label} exceeded {limit} byte response limit"));
    }
    let capacity = response.content_length().and_then(|length| usize::try_from(length).ok()).unwrap_or(0).min(limit);
    let mut output = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("read {label}: {error}"))?;
        let next = output.len().checked_add(chunk.len()).ok_or_else(|| format!("{label} response size overflow"))?;
        if next > limit {
            return Err(format!("{label} exceeded {limit} byte response limit"));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

pub(crate) async fn text(response: reqwest::Response, limit: usize, label: &str) -> Result<String, String> {
    let body = bytes(response, limit, label).await?;
    String::from_utf8(body).map_err(|error| format!("{label} was not valid UTF-8: {error}"))
}

pub(crate) async fn text_lossy(response: reqwest::Response, limit: usize, label: &str) -> Result<String, String> {
    bytes(response, limit, label).await.map(|body| String::from_utf8_lossy(&body).into_owned())
}

pub(crate) async fn json<T: DeserializeOwned>(response: reqwest::Response, limit: usize, label: &str) -> Result<T, String> {
    let body = bytes(response, limit, label).await?;
    serde_json::from_slice(&body).map_err(|error| format!("{label} contained invalid JSON: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn serve(response: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request);
            socket.write_all(&response).unwrap();
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn rejects_declared_and_chunked_bodies_over_limit() {
        let declared = serve(b"HTTP/1.1 200 OK\r\ncontent-length: 9999\r\nconnection: close\r\n\r\n".to_vec());
        let response = reqwest::get(declared).await.unwrap();
        assert!(bytes(response, 8, "declared").await.unwrap_err().contains("exceeded"));

        let chunked = serve(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n6\r\nabcdef\r\n6\r\nghijkl\r\n0\r\n\r\n".to_vec(),
        );
        let response = reqwest::get(chunked).await.unwrap();
        assert!(bytes(response, 8, "chunked").await.unwrap_err().contains("exceeded"));
    }
}

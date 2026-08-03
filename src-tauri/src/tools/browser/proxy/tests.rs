use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct FixedResolver {
    addresses: Vec<SocketAddr>,
    calls: AtomicUsize,
}

impl FixedResolver {
    fn new(addresses: Vec<SocketAddr>) -> Self {
        Self { addresses, calls: AtomicUsize::new(0) }
    }
}

impl Resolver for FixedResolver {
    fn resolve<'a>(&'a self, _host: &'a str, port: u16) -> ResolveFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(self.addresses.iter().map(|address| SocketAddr::new(address.ip(), port)).collect()) })
    }
}

async fn exchange(addr: SocketAddr, request: &[u8]) -> String {
    let mut client = TcpStream::connect(addr).await.unwrap();
    client.write_all(request).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut response)).await.unwrap().unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn connect_to_private_target_is_rejected_before_upstream_connect() {
    let mut proxy = BrowserProxy::launch().await.unwrap();
    let response = exchange(proxy.addr(), b"CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(response.contains("blocked address"), "{response}");
    proxy.close().await;
}

#[tokio::test]
async fn absolute_http_request_to_private_target_is_rejected() {
    let mut proxy = BrowserProxy::launch().await.unwrap();
    let response = exchange(proxy.addr(), b"GET http://169.254.169.254/latest HTTP/1.1\r\nHost: 169.254.169.254\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert!(response.contains("blocked address"), "{response}");
    proxy.close().await;
}

#[tokio::test]
async fn every_dns_result_must_be_public() {
    let resolver = Arc::new(FixedResolver::new(vec![SocketAddr::from(([1, 1, 1, 1], 1)), SocketAddr::from(([127, 0, 0, 1], 1))]));
    let mut proxy = BrowserProxy::launch_with(resolver.clone(), false).await.unwrap();
    let response = exchange(proxy.addr(), b"CONNECT mixed.test:443 HTTP/1.1\r\nHost: mixed.test:443\r\n\r\n").await;
    assert!(response.starts_with("HTTP/1.1 403"), "{response}");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    proxy.close().await;
}

#[test]
fn reserved_and_non_unicast_addresses_are_rejected() {
    for address in [
        "0.0.0.1:80",
        "192.0.2.1:80",
        "198.18.0.1:80",
        "203.0.113.1:80",
        "224.0.0.1:80",
        "240.0.0.1:80",
        "[100::]:80",
        "[2001:db8::1]:80",
        "[fec0::1]:80",
        "[64:ff9b:1::1]:80",
        "[64:ff9b::127.0.0.1]:80",
    ] {
        let address = address.parse::<SocketAddr>().unwrap();
        assert!(validate_addresses("reserved.test", &[address], false).is_err(), "{address}");
    }
    assert!(validate_addresses("public.test", &["1.1.1.1:443".parse().unwrap()], false).is_ok());
    assert!(validate_addresses("nat64.test", &["[64:ff9b::1.1.1.1]:443".parse().unwrap()], false).is_ok());
}

#[tokio::test]
async fn validated_resolution_is_pinned_for_the_origin_connection() {
    let origin = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let resolver = Arc::new(FixedResolver::new(vec![origin_addr]));
    let mut proxy = BrowserProxy::launch_with(resolver.clone(), true).await.unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut received = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
            received.extend_from_slice(&chunk[..count]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8(received).unwrap();
        assert!(text.starts_with("GET /pinned?q=1 HTTP/1.1\r\n"), "{text}");
        assert!(text.contains(&format!("Host: public.test:{}\r\n", origin_addr.port())), "{text}");
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK").await.unwrap();
    });
    let request =
        format!("GET http://public.test:{}/pinned?q=1 HTTP/1.1\r\nHost: public.test:{}\r\n\r\n", origin_addr.port(), origin_addr.port());
    let response = exchange(proxy.addr(), request.as_bytes()).await;
    assert!(response.ends_with("OK"), "{response}");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    origin_task.await.unwrap();
    proxy.close().await;
}

#[tokio::test]
async fn connect_tunnel_uses_pinned_address_and_preserves_hostname_boundary() {
    let origin = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let resolver = Arc::new(FixedResolver::new(vec![origin_addr]));
    let mut proxy = BrowserProxy::launch_with(resolver.clone(), true).await.unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut ping = [0_u8; 4];
        stream.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"PING");
        stream.write_all(b"PONG").await.unwrap();
    });
    let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
    let authority = format!("secure.test:{}", origin_addr.port());
    client.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        response.push(byte[0]);
        if response.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    assert!(response.starts_with(b"HTTP/1.1 200"), "{}", String::from_utf8_lossy(&response));
    client.write_all(b"PING").await.unwrap();
    let mut pong = [0_u8; 4];
    client.read_exact(&mut pong).await.unwrap();
    assert_eq!(&pong, b"PONG");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    origin_task.await.unwrap();
    proxy.close().await;
}

#[tokio::test]
async fn forward_connection_never_blindly_accepts_a_second_target() {
    let origin = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let resolver = Arc::new(FixedResolver::new(vec![origin_addr]));
    let mut proxy = BrowserProxy::launch_with(resolver, true).await.unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut first = Vec::new();
        loop {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            first.push(byte[0]);
            if first.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&first).starts_with("GET /first HTTP/1.1\r\n"));
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nOK").await.unwrap();
        let mut next = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut next)).await.unwrap().unwrap();
        assert_eq!(read, 0, "a second absolute-form request reached the first origin");
    });
    let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
    let first = format!("GET http://public.test:{}/first HTTP/1.1\r\nHost: public.test:{}\r\n\r\n", origin_addr.port(), origin_addr.port());
    client.write_all(first.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\nOK") {
        let mut byte = [0_u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        response.push(byte[0]);
    }
    client.write_all(b"GET http://127.0.0.1/private HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await.unwrap();
    origin_task.await.unwrap();
    proxy.close().await;
}

#[tokio::test]
async fn validated_websocket_upgrade_becomes_a_fixed_target_tunnel() {
    let origin = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    let resolver = Arc::new(FixedResolver::new(vec![origin_addr]));
    let mut proxy = BrowserProxy::launch_with(resolver.clone(), true).await.unwrap();
    let origin_task = tokio::spawn(async move {
        let (mut stream, _) = origin.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
        }
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("GET /socket HTTP/1.1\r\n"), "{text}");
        assert!(text.contains("Connection: Upgrade\r\n"), "{text}");
        stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n").await.unwrap();
        let mut ping = [0_u8; 4];
        stream.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"PING");
        stream.write_all(b"PONG").await.unwrap();
    });
    let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
    let authority = format!("socket.test:{}", origin_addr.port());
    client
        .write_all(
            format!("GET http://{authority}/socket HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        client.read_exact(&mut byte).await.unwrap();
        response.push(byte[0]);
    }
    assert!(response.starts_with(b"HTTP/1.1 101"));
    client.write_all(b"PING").await.unwrap();
    let mut pong = [0_u8; 4];
    client.read_exact(&mut pong).await.unwrap();
    assert_eq!(&pong, b"PONG");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    origin_task.await.unwrap();
    proxy.close().await;
}

#[tokio::test]
async fn oversized_headers_fail_closed() {
    let mut proxy = BrowserProxy::launch().await.unwrap();
    let request =
        format!("GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nX-Fill: {}\r\n\r\n", "a".repeat(request::MAX_HEADER_BYTES));
    let response = exchange(proxy.addr(), request.as_bytes()).await;
    assert!(response.starts_with("HTTP/1.1 431"), "{response}");
    proxy.close().await;
}

#[tokio::test]
async fn shutdown_closes_listener_and_active_connections() {
    let mut proxy = BrowserProxy::launch().await.unwrap();
    let addr = proxy.addr();
    let mut active = TcpStream::connect(addr).await.unwrap();
    proxy.close().await;
    let mut byte = [0_u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(1), active.read(&mut byte)).await.unwrap();
    assert!(matches!(closed, Ok(0) | Err(_)), "active proxy connection remained readable: {closed:?}");
    assert!(TcpStream::connect(addr).await.is_err());
}

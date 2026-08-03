//! Chrome 专用的 loopback forward proxy。
//! Chrome 只知道 proxy 地址；目标 DNS、地址校验和 TCP 连接全部在这里完成，因此没有
//! preflight 与实际连接之间的 DNS rebinding 窗口。

mod connection;
mod request;

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DNS_RESULTS: usize = 16;

pub(super) struct BrowserProxy {
    addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl BrowserProxy {
    pub(super) async fn launch() -> Result<Self, String> {
        Self::launch_with(Arc::new(SystemResolver), false).await
    }

    async fn launch_with(resolver: Arc<dyn Resolver>, allow_loopback: bool) -> Result<Self, String> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| format!("failed to bind browser proxy: {error}"))?;
        let addr = listener.local_addr().map_err(|error| format!("failed to read browser proxy address: {error}"))?;
        let (shutdown, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(connection::serve(listener, receiver, resolver, allow_loopback));
        Ok(Self { addr, shutdown: Some(shutdown), task })
    }

    pub(super) fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub(super) async fn close(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut self.task).await.is_err() {
            self.task.abort();
        }
    }
}

impl Drop for BrowserProxy {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

type ResolveFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, String>> + Send + 'a>>;

trait Resolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            let lookup = tokio::net::lookup_host((host, port)).await.map_err(|error| format!("dns resolve failed for {host}: {error}"))?;
            Ok(lookup.take(MAX_DNS_RESULTS + 1).collect())
        })
    }
}

async fn connect_target(resolver: &dyn Resolver, target: &request::Target, allow_loopback: bool) -> Result<TcpStream, String> {
    let addresses = match target.host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, target.port)],
        Err(_) => tokio::time::timeout(DNS_TIMEOUT, resolver.resolve(&target.host, target.port))
            .await
            .map_err(|_| format!("dns resolve timed out for {}", target.host))??,
    };
    validate_addresses(&target.host, &addresses, allow_loopback)?;
    tokio::time::timeout(CONNECT_TIMEOUT, async {
        let mut last = None;
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last = Some(error),
            }
        }
        Err(format!(
            "failed to connect to {}:{}: {}",
            target.host,
            target.port,
            last.map(|error| error.to_string()).unwrap_or_else(|| "no address".into())
        ))
    })
    .await
    .map_err(|_| format!("connect timed out for {}:{}", target.host, target.port))?
}

fn validate_addresses(host: &str, addresses: &[SocketAddr], allow_loopback: bool) -> Result<(), String> {
    if addresses.is_empty() {
        return Err(format!("dns resolve failed for {host}: no address"));
    }
    if addresses.len() > MAX_DNS_RESULTS {
        return Err(format!("dns resolve failed for {host}: more than {MAX_DNS_RESULTS} addresses"));
    }
    for address in addresses {
        let ip = address.ip();
        let blocked = crate::tools::net_guard::is_blocked_ip(&ip) && !(allow_loopback && ip.is_loopback());
        if blocked || is_additional_non_public(ip) {
            return Err(format!("{host} resolves to blocked address {ip}"));
        }
    }
    Ok(())
}

fn is_additional_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            a == 0
                || a >= 224
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && matches!(b, 18 | 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
        }
        IpAddr::V6(ip) => {
            let octets = ip.octets();
            let segments = ip.segments();
            let site_local = octets[0] == 0xfe && (0xc0..=0xff).contains(&octets[1]);
            let discard_only = segments[0] == 0x0100 && segments[1..4].iter().all(|segment| *segment == 0);
            let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
            let benchmark = segments[0] == 0x2001 && segments[1] == 0x0002 && segments[2] == 0;
            let local_nat64 = segments[0] == 0x0064 && segments[1] == 0xff9b && segments[2] == 0x0001;
            let well_known_nat64 = octets[..12] == [0, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0];
            let nat64_private = well_known_nat64 && {
                let embedded = std::net::Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]);
                let embedded = IpAddr::V4(embedded);
                crate::tools::net_guard::is_blocked_ip(&embedded) || is_additional_non_public(embedded)
            };
            ip.is_multicast() || site_local || discard_only || documentation || benchmark || local_nat64 || nat64_private
        }
    }
}

#[cfg(test)]
mod tests;

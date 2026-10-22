//! How relay connections leave this PC: directly, or through a local Tor
//! SOCKS5 proxy (Tor Browser listens on 9150, the Tor service on 9050).
//! Relay host names go to the proxy unresolved, so DNS stays inside Tor,
//! and a proxy that is down means no relay traffic, never a direct fallback.

use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Tor Browser's SOCKS port, the usual case on a desktop.
pub const TOR_BROWSER_PROXY: &str = "127.0.0.1:9150";
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transport {
    #[default]
    Direct,
    Tor(SocketAddr),
}

impl Transport {
    /// `None` (off) or a `host:port` SOCKS address; anything else is refused.
    pub fn from_setting(proxy: Option<&str>) -> Result<Self, String> {
        match proxy.map(str::trim).filter(|p| !p.is_empty()) {
            None => Ok(Self::Direct),
            Some(addr) => addr
                .parse()
                .map(Self::Tor)
                .map_err(|_| format!("{addr} is not a proxy address like {TOR_BROWSER_PROXY}")),
        }
    }

    pub fn is_tor(&self) -> bool {
        matches!(self, Self::Tor(_))
    }

    /// No NIP-42 AUTH (no authenticator), as iOS: answering a challenge
    /// proves the key to the relay and adds nothing phones can reach. Events
    /// are signed by the caller, so the client holds no key.
    pub(crate) fn client(&self) -> Client {
        match self {
            Self::Direct => Client::builder().build(),
            Self::Tor(addr) => Client::builder().proxy(Proxy::all(*addr)).build(),
        }
    }
}

/// True when a SOCKS5 server answers at `addr` and accepts no-auth, as Tor
/// does. Says nothing about whether Tor has finished bootstrapping.
pub async fn socks_ready(addr: SocketAddr) -> bool {
    let attempt = async {
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await?;
        Ok::<_, std::io::Error>(reply == [0x05, 0x00])
    };
    matches!(tokio::time::timeout(PROBE_TIMEOUT, attempt).await, Ok(Ok(true)))
}

/// Tor as relay traffic sees it. An idle Tor (or one whose PC slept) still
/// answers on its port while it refetches the network and builds circuits,
/// which can take minutes; streams opened meanwhile just wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorHealth {
    /// Nothing answers at the proxy address.
    Down,
    /// The proxy answers but cannot reach the internet yet.
    Building,
    /// A stream through Tor reached a relay.
    Ready,
}

/// True when the SOCKS5 proxy at `addr` opens a stream to `host:443`
/// within `timeout`. The host goes to the proxy by name, so DNS stays in Tor.
pub async fn tor_circuit_ready(addr: SocketAddr, host: &str, timeout: Duration) -> bool {
    let Ok(name_len) = u8::try_from(host.len()) else {
        return false;
    };
    let attempt = async {
        let mut stream = TcpStream::connect(addr).await?;
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
        let mut hello = [0u8; 2];
        stream.read_exact(&mut hello).await?;
        if hello != [0x05, 0x00] {
            return Ok(false);
        }
        let mut request = vec![0x05, 0x01, 0x00, 0x03, name_len];
        request.extend_from_slice(host.as_bytes());
        request.extend_from_slice(&443u16.to_be_bytes());
        stream.write_all(&request).await?;
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await?;
        Ok::<_, std::io::Error>(reply == [0x05, 0x00])
    };
    matches!(tokio::time::timeout(timeout, attempt).await, Ok(Ok(true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn settings_parse_to_a_transport() {
        assert_eq!(Transport::from_setting(None), Ok(Transport::Direct));
        assert_eq!(Transport::from_setting(Some("  ")), Ok(Transport::Direct));
        assert_eq!(
            Transport::from_setting(Some(TOR_BROWSER_PROXY)),
            Ok(Transport::Tor("127.0.0.1:9150".parse().unwrap()))
        );
        assert!(Transport::from_setting(Some("localhost:9050")).is_err());
    }

    #[tokio::test]
    async fn probe_accepts_socks5_and_rejects_others() {
        let socks = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socks.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = socks.accept().await.unwrap();
            let mut hello = [0u8; 3];
            s.read_exact(&mut hello).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
        });
        assert!(socks_ready(addr).await);

        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = http.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = http.accept().await.unwrap();
            s.write_all(b"HTTP/1.1 400\r\n\r\n").await.unwrap();
        });
        assert!(!socks_ready(addr).await);

        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        assert!(!socks_ready(closed).await);
    }

    /// A SOCKS5 proxy that answers every CONNECT with `code`, as Tor answers
    /// 0x00 once a circuit carries the stream and an error when it cannot.
    async fn answering_proxy(code: u8) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut hello = [0u8; 3];
                    s.read_exact(&mut hello).await?;
                    s.write_all(&[0x05, 0x00]).await?;
                    let mut head = [0u8; 5];
                    s.read_exact(&mut head).await?;
                    let mut rest = vec![0u8; head[4] as usize + 2];
                    s.read_exact(&mut rest).await?;
                    s.write_all(&[0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
                    Ok::<_, std::io::Error>(())
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn circuit_probe_needs_a_stream_not_just_a_port() {
        let timeout = Duration::from_secs(2);
        assert!(tor_circuit_ready(answering_proxy(0x00).await, "relay.damus.io", timeout).await);
        // 0x04: host unreachable, what Tor says when it gives up on a circuit.
        assert!(!tor_circuit_ready(answering_proxy(0x04).await, "relay.damus.io", timeout).await);

        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = silent.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = silent.accept().await.unwrap();
            let mut hello = [0u8; 3];
            s.read_exact(&mut hello).await.unwrap();
            s.write_all(&[0x05, 0x00]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let started = std::time::Instant::now();
        assert!(!tor_circuit_ready(addr, "relay.damus.io", Duration::from_millis(300)).await);
        assert!(started.elapsed() < Duration::from_secs(2), "a waiting circuit must time out");
    }

    /// `cargo test -p witchat-nostr live_tor -- --ignored` with the tor service on 9050.
    #[tokio::test]
    #[ignore]
    async fn live_tor_reaches_a_relay() {
        let proxy: SocketAddr = "127.0.0.1:9050".parse().unwrap();
        assert!(socks_ready(proxy).await, "nothing answers at {proxy}");
        let started = std::time::Instant::now();
        assert!(tor_circuit_ready(proxy, "relay.damus.io", Duration::from_secs(30)).await);
        println!("circuit stream opened in {:?}", started.elapsed());
    }

    /// A fake SOCKS5 proxy that records each CONNECT target, then refuses it.
    async fn recording_proxy() -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<(u8, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut head = [0u8; 2];
                    s.read_exact(&mut head).await?;
                    let mut methods = vec![0u8; head[1] as usize];
                    s.read_exact(&mut methods).await?;
                    s.write_all(&[0x05, 0x00]).await?;
                    let mut req = [0u8; 4];
                    s.read_exact(&mut req).await?;
                    let host = match req[3] {
                        0x03 => {
                            let mut len = [0u8; 1];
                            s.read_exact(&mut len).await?;
                            let mut name = vec![0u8; len[0] as usize];
                            s.read_exact(&mut name).await?;
                            String::from_utf8_lossy(&name).into_owned()
                        }
                        other => format!("address type {other}"),
                    };
                    let _ = tx.send((req[3], host));
                    // 0x02: connection not allowed by ruleset.
                    s.write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
                    Ok::<_, std::io::Error>(())
                });
            }
        });
        (addr, rx)
    }

    #[tokio::test]
    async fn relays_go_through_the_proxy_by_name() {
        let (proxy, mut seen) = recording_proxy().await;
        let cell = "9q8yy";
        let connecting =
            tokio::spawn(
                async move { crate::GeoChannel::connect(cell, Keys::generate(), Transport::Tor(proxy)).await },
            );
        let expected: Vec<String> = crate::geo::relays_for(cell)
            .iter()
            .filter_map(|r| r.split("://").nth(1).map(|h| h.split([':', '/']).next().unwrap_or(h).to_string()))
            .collect();
        let (kind, host) = tokio::time::timeout(Duration::from_secs(10), seen.recv())
            .await
            .expect("no connection reached the proxy")
            .unwrap();
        connecting.abort();
        assert_eq!(kind, 0x03, "host must be sent as a name so DNS happens inside tor");
        assert!(expected.contains(&host), "{host} is not one of the cell's relays");
    }
}

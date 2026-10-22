//! One HTTPS GET for small documents (the relay directory), over the same
//! route as the relays: direct, or through the Tor SOCKS proxy with the
//! host name resolved inside Tor. A proxy that is down fails the request;
//! there is no direct fallback. Redirects are refused, as iOS requires the
//! response to come from the URL it asked for.

use crate::transport::Transport;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

const HEADER_LIMIT: usize = 16 * 1024;

/// Body of `https://{host}{path}` if the server answers 200 within
/// `timeout` and sends at most `max_body` bytes.
pub async fn get(
    host: &str,
    path: &str,
    transport: Transport,
    max_body: usize,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    tokio::time::timeout(timeout, fetch(host, path, transport, max_body))
        .await
        .map_err(|_| format!("{host}: timed out after {}s", timeout.as_secs()))?
}

async fn fetch(host: &str, path: &str, transport: Transport, max_body: usize) -> Result<Vec<u8>, String> {
    let tcp = match transport {
        Transport::Direct => TcpStream::connect((host, 443)).await.map_err(|e| format!("{host}: {e}"))?,
        Transport::Tor(proxy) => tokio_socks::tcp::Socks5Stream::connect(proxy, (host, 443))
            .await
            .map_err(|e| format!("{host} via tor: {e}"))?
            .into_inner(),
    };
    let name = ServerName::try_from(host.to_string()).map_err(|e| format!("{host}: {e}"))?;
    let tls = connector().connect(name, tcp).await.map_err(|e| format!("{host}: tls: {e}"))?;
    exchange(tls, host, path, max_body).await
}

fn connector() -> TlsConnector {
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    host: &str,
    path: &str,
    max_body: usize,
) -> Result<Vec<u8>, String> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: witchat\r\nAccept: text/csv, text/plain\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
    let cap = HEADER_LIMIT + max_body + 64 * 1024;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > cap {
                    return Err(format!("{host}: response larger than {max_body} bytes"));
                }
            }
            // Servers often close without a TLS close_notify; the length
            // checks in `parse_response` still catch a cut-off body.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => break,
            Err(e) => return Err(format!("{host}: {e}")),
        }
    }
    parse_response(&raw, max_body)
}

/// Status 200 body from a raw HTTP/1.1 response, de-chunked.
pub(crate) fn parse_response(raw: &[u8], max_body: usize) -> Result<Vec<u8>, String> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no HTTP header")?;
    if split > HEADER_LIMIT {
        return Err("HTTP header too large".into());
    }
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "HTTP header is not text")?;
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code = status.split_whitespace().nth(1).unwrap_or_default();
    if code != "200" {
        return Err(format!("HTTP {status}"));
    }
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        if name == "transfer-encoding" && value.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        } else if name == "content-length" {
            length = Some(value.parse::<usize>().map_err(|_| "bad Content-Length")?);
        } else if name == "content-encoding" && !value.eq_ignore_ascii_case("identity") {
            return Err(format!("unexpected Content-Encoding {value}"));
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(length) = length {
        if body.len() < length {
            return Err("body cut short".into());
        }
        body[..length].to_vec()
    } else {
        body.to_vec()
    };
    if body.len() > max_body {
        return Err(format!("body larger than {max_body} bytes"));
    }
    Ok(body)
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = data.windows(2).position(|w| w == b"\r\n").ok_or("chunk size line missing")?;
        let size_line = std::str::from_utf8(&data[..end]).map_err(|_| "bad chunk size")?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "bad chunk size")?;
        data = &data[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            return Err("chunk cut short".into());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_chunked_bodies_parse() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_response(plain, 100).unwrap(), b"hello");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2;x=y\r\nlo\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked, 100).unwrap(), b"hello");
    }

    #[test]
    fn redirects_errors_and_oversize_are_refused() {
        let moved = b"HTTP/1.1 301 Moved\r\nLocation: https://evil.example/\r\n\r\n";
        assert!(parse_response(moved, 100).is_err());
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nhello";
        assert!(parse_response(short, 100).is_err());
        let big = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert!(parse_response(big, 4).is_err());
        let gzip = b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 1\r\n\r\nx";
        assert!(parse_response(gzip, 100).is_err());
    }
}

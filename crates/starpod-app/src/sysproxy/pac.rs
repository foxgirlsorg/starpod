//! The PAC script server.
//!
//! In PAC mode the system is told to fetch its proxy configuration from a
//! loopback address. This server answers that one request with the script.
//! It listens only on 127.0.0.1, on a port the operating system picks.

use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The path the system requests.
pub const PAC_PATH: &str = "/pac";

/// The MIME type a system expects for a PAC script.
pub const PAC_CONTENT_TYPE: &str = "application/x-ns-proxy-autoconfig";

/// The placeholder in a PAC script that stands for the engine's mixed port.
pub const PORT_PLACEHOLDER: &str = "%mixed-port%";

/// The most request bytes read. A PAC request is a single short line.
const MAX_REQUEST_BYTES: usize = 4096;

/// How long one connection may take to send its request or read the reply.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Fill in the engine's mixed port.
pub fn render(script: &str, mixed_port: u16) -> String {
    script.replace(PORT_PLACEHOLDER, &mixed_port.to_string())
}

/// The reply to one request. A `GET /pac` gets the script; anything else gets
/// a 404.
pub fn response(request: &[u8], body: &str) -> Vec<u8> {
    let head = String::from_utf8_lossy(request);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next();
    let target = parts.next().unwrap_or("");
    let path = target.split('?').next().unwrap_or("");
    if method == Some("GET") && path == PAC_PATH {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {PAC_CONTENT_TYPE}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    } else {
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
    }
}

/// A running PAC server. Dropping it stops the listener.
#[derive(Debug)]
pub struct PacServer {
    port: u16,
    task: JoinHandle<()>,
}

impl PacServer {
    /// Serve `body` on a free loopback port.
    pub async fn start(body: String) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let body = Arc::new(body);
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let body = Arc::clone(&body);
                        tokio::spawn(async move { serve(stream, &body).await });
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        });
        Ok(Self { port, task })
    }

    /// The address the system fetches the script from.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{PAC_PATH}", self.port)
    }
}

impl Drop for PacServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, body: &str) {
    let mut buffer = vec![0u8; MAX_REQUEST_BYTES];
    let read = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut buffer)).await {
        Ok(Ok(count)) => count,
        _ => return,
    };
    let reply = response(&buffer[..read], body);
    let _ = tokio::time::timeout(IO_TIMEOUT, async {
        stream.write_all(&reply).await?;
        stream.shutdown().await
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mixed_port_replaces_every_placeholder() {
        let script = "PROXY 127.0.0.1:%mixed-port%; DIRECT /* %mixed-port% */";
        assert_eq!(
            render(script, 7890),
            "PROXY 127.0.0.1:7890; DIRECT /* 7890 */"
        );
    }

    #[test]
    fn the_default_script_renders_with_the_engine_port_and_no_placeholder_left() {
        let rendered = render(&starpod_core::domain::default_pac_script(), 7890);
        assert!(rendered.contains("return \"PROXY 127.0.0.1:7890; DIRECT\";"));
        assert!(!rendered.contains(PORT_PLACEHOLDER));
        assert!(rendered.contains("FindProxyForURL"));
    }

    #[test]
    fn a_script_without_the_placeholder_is_unchanged() {
        assert_eq!(render("return \"DIRECT\";", 1), "return \"DIRECT\";");
    }

    #[test]
    fn get_pac_returns_the_script_with_the_pac_content_type() {
        let reply = response(b"GET /pac HTTP/1.1\r\nHost: x\r\n\r\n", "BODY");
        let text = String::from_utf8(reply).unwrap();

        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Type: application/x-ns-proxy-autoconfig\r\n"));
        assert!(text.contains("Content-Length: 4\r\n"));
        assert!(text.ends_with("\r\n\r\nBODY"));
    }

    #[test]
    fn a_query_string_on_the_pac_path_still_gets_the_script() {
        let reply = response(b"GET /pac?v=2 HTTP/1.1\r\n\r\n", "B");
        assert!(
            String::from_utf8(reply)
                .unwrap()
                .starts_with("HTTP/1.1 200 OK")
        );
    }

    #[test]
    fn any_other_request_gets_a_404() {
        let post = response(b"POST /pac HTTP/1.1\r\n\r\n", "B");
        let other = response(b"GET /other HTTP/1.1\r\n\r\n", "B");
        assert!(String::from_utf8(post).unwrap().starts_with("HTTP/1.1 404"));
        assert!(
            String::from_utf8(other)
                .unwrap()
                .starts_with("HTTP/1.1 404")
        );
    }

    #[tokio::test]
    async fn the_server_answers_a_request_over_loopback() {
        let server = PacServer::start("PROXY 127.0.0.1:7890; DIRECT".to_string())
            .await
            .unwrap();
        let url = server.url();
        assert!(url.starts_with("http://127.0.0.1:"));
        assert!(url.ends_with(PAC_PATH));

        let port: u16 = url
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches(PAC_PATH)
            .parse()
            .unwrap();
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        stream
            .write_all(b"GET /pac HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();

        assert!(reply.starts_with("HTTP/1.1 200 OK"));
        assert!(reply.ends_with("PROXY 127.0.0.1:7890; DIRECT"));
    }
}

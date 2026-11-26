use std::{collections::HashMap, fmt, time::Duration};

use bytes::Bytes;
use nautilus_network::http::{HttpClient, HttpRedirectPolicy, Method};
use tokio::time::{Instant, sleep_until};

use crate::{
    auth::TokenManager,
    env::{Env, LiveDisabled},
};

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("invalid request")]
    InvalidRequest,
    #[error("authentication failed")]
    Authentication,
    #[error("transport failed")]
    Transport,
}

pub struct Reply {
    pub status: u16,
    pub body: Bytes,
}
impl fmt::Debug for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reply")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}
pub enum WriteOutcome {
    Answered { status: u16, body: Bytes },
    Unknown,
}

impl fmt::Debug for WriteOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Answered { status, body } => f
                .debug_struct("Answered")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

pub struct SaxoHttp {
    client: HttpClient,
    base: String,
    last_write: Option<Instant>,
}
impl SaxoHttp {
    pub fn new(env: Env) -> Result<Self, LiveDisabled> {
        let base = env.base_url()?;
        Self::build(base, 20).map_err(|_| LiveDisabled)
    }
    fn build(base: &str, timeout: u64) -> Result<Self, HttpError> {
        let client = HttpClient::builder()
            .use_system_proxy(false)
            .redirect_policy(HttpRedirectPolicy::Reject)
            .timeout_secs(timeout)
            .redact_url(true)
            .build()
            .map_err(|_| HttpError::Transport)?;
        Ok(Self {
            client,
            base: base.to_owned(),
            last_write: None,
        })
    }
    #[cfg(test)]
    pub(crate) fn test_client(base: &str) -> Self {
        Self::build(base, 1).expect("client")
    }
    pub async fn get(&self, tokens: &mut TokenManager, path: &str) -> Result<Reply, HttpError> {
        if !valid_path(path) {
            return Err(HttpError::InvalidRequest);
        }
        let access = tokens
            .access(Instant::now())
            .await
            .map_err(|_| HttpError::Authentication)?;
        let headers = HashMap::from([("authorization".to_owned(), format!("Bearer {access}"))]);
        let response = self
            .client
            .request(
                Method::GET,
                format!("{}{path}", self.base),
                None,
                Some(headers),
                None,
                None,
                None,
            )
            .await
            .map_err(|_| HttpError::Transport)?;
        Ok(Reply {
            status: response.status.as_u16(),
            body: response.body,
        })
    }
    pub async fn write(
        &mut self,
        tokens: &mut TokenManager,
        method: Method,
        path: &str,
        body: Bytes,
        request_id: &str,
    ) -> Result<WriteOutcome, HttpError> {
        if !valid_path(path)
            || !allowed(&method, path)
            || request_id.is_empty()
            || request_id.len() > 50
            || !request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(HttpError::InvalidRequest);
        }
        if let Some(last) = self.last_write {
            sleep_until(last + Duration::from_millis(1100)).await;
        }
        let access = tokens
            .access(Instant::now())
            .await
            .map_err(|_| HttpError::Authentication)?;
        let headers = HashMap::from([
            ("authorization".to_owned(), format!("Bearer {access}")),
            ("x-request-id".to_owned(), request_id.to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
        ]);
        let response = self
            .client
            .request(
                method,
                format!("{}{path}", self.base),
                None,
                Some(headers),
                Some(body.to_vec()),
                None,
                None,
            )
            .await;
        self.last_write = Some(Instant::now());
        Ok(match response {
            Ok(reply) => WriteOutcome::Answered {
                status: reply.status.as_u16(),
                body: reply.body,
            },
            Err(_) => WriteOutcome::Unknown,
        })
    }
}
fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains("..")
        && !path.contains("//")
        && !path.split('?').next().unwrap_or("").contains('%')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_?&=.,%-".contains(&b))
}
fn numeric(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}
fn allowed(method: &Method, path: &str) -> bool {
    if (method == Method::POST || method == Method::PATCH) && path == "/trade/v2/orders" {
        return true;
    }
    if method == Method::DELETE
        && let Some(rest) = path.strip_prefix("/trade/v2/orders/")
        && let Some((id, account)) = rest.split_once("?AccountKey=")
    {
        return numeric(id) && !account.is_empty() && !account.contains(['&', '?', '=']);
    }
    if method == Method::PATCH
        && let Some(id) = path.strip_prefix("/trade/v1/positions/")
    {
        return numeric(id);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    use super::test_support::server;
    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    #[tokio::test]
    async fn rejects_invalid_requests_before_sending() {
        let (base, mut rx, task) = server(OK, false).await;
        let mut client = SaxoHttp::test_client(&base);
        let mut tokens = TokenManager::test_token();
        for (method, path) in [
            (Method::POST, "/port/v1/orders"),
            (Method::DELETE, "/trade/v2/orders/abc?AccountKey=x"),
            (Method::POST, "/trade/v2/orders?x=y"),
            (Method::PATCH, "/trade/v1/positions/3?bad=x"),
        ] {
            assert!(
                client
                    .write(&mut tokens, method, path, Bytes::new(), "id")
                    .await
                    .is_err()
            );
        }
        for path in ["/../x", "//evil", "/x y", "/x#y", "/x/%2e%2e/y"] {
            assert!(client.get(&mut tokens, path).await.is_err());
        }
        assert!(
            client
                .write(
                    &mut tokens,
                    Method::POST,
                    "/trade/v2/orders",
                    Bytes::new(),
                    "bad_id"
                )
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(100), rx.recv())
                .await
                .is_err()
        );
        task.abort();
    }
    #[tokio::test]
    async fn writes_are_spaced_and_identified() {
        let (base, mut rx, task) = server(OK, false).await;
        let mut client = SaxoHttp::test_client(&base);
        let mut tokens = TokenManager::test_token();
        for method in [Method::POST, Method::PATCH] {
            assert!(matches!(
                client
                    .write(
                        &mut tokens,
                        method,
                        "/trade/v2/orders",
                        Bytes::from_static(b"{}"),
                        "id-1"
                    )
                    .await
                    .expect("write"),
                WriteOutcome::Answered { status: 200, .. }
            ));
        }
        let (first, request) = rx.recv().await.expect("first");
        let (second, _) = rx.recv().await.expect("second");
        assert!(second.duration_since(first) >= Duration::from_millis(1100));
        assert!(request.to_ascii_lowercase().contains("x-request-id: id-1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        );
        task.abort();
    }
    #[tokio::test]
    async fn failures_are_not_retried_and_conflict_is_unknown_to_parser() {
        for (response, hang, status) in [
            (
                "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                false,
                Some(500),
            ),
            (OK, true, None),
            (
                "HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                false,
                Some(409),
            ),
        ] {
            let (base, mut rx, task) = server(response, hang).await;
            let mut client = SaxoHttp::test_client(&base);
            let result = client
                .write(
                    &mut TokenManager::test_token(),
                    Method::POST,
                    "/trade/v2/orders",
                    Bytes::new(),
                    "id",
                )
                .await
                .expect("write");
            match (result, status) {
                (WriteOutcome::Answered { status, body }, Some(expected)) => {
                    assert_eq!(status, expected);
                    if status == 409 {
                        assert!(matches!(
                            crate::parse::placement(
                                status,
                                std::str::from_utf8(&body).expect("utf8")
                            )
                            .expect("placement"),
                            crate::parse::Placement::Unknown
                        ));
                    }
                }
                (WriteOutcome::Unknown, None) => (),
                _ => panic!("unexpected outcome"),
            }
            assert!(rx.recv().await.is_some());
            assert!(
                timeout(Duration::from_millis(150), rx.recv())
                    .await
                    .is_err()
            );
            task.abort();
        }
    }
    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (base, mut rx, task) = server("HTTP/1.1 302 Found\r\nLocation: /redirected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", false).await;
        let client = SaxoHttp::test_client(&base);
        let reply = client
            .get(&mut TokenManager::test_token(), "/path")
            .await
            .expect("get");
        assert_eq!(reply.status, 302);
        assert!(rx.recv().await.is_some());
        assert!(
            timeout(Duration::from_millis(150), rx.recv())
                .await
                .is_err()
        );
        assert!(
            !format!(
                "{:?}",
                Reply {
                    status: 200,
                    body: Bytes::from_static(b"secret")
                }
            )
            .contains("secret")
        );
        assert!(SaxoHttp::new(Env::Live).is_err());
        task.abort();
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::mpsc,
        time::Instant,
    };
    pub(crate) async fn server(
        response: &'static str,
        hang: bool,
    ) -> (
        String,
        mpsc::UnboundedReceiver<(Instant, String)>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let tx = tx.clone();
                tokio::spawn(async move {
                    let now = Instant::now();
                    let mut buf = Vec::new();
                    loop {
                        let mut part = [0; 4096];
                        let n = stream.read(&mut part).await.expect("read");
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&part[..n]);
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&buf[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length: ")
                                        .and_then(|v| v.parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let _ = tx.send((now, String::from_utf8_lossy(&buf).into_owned()));
                    if hang {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    } else {
                        let _ = stream.write_all(response.as_bytes()).await;
                    }
                });
            }
        });
        (base, rx, task)
    }
}

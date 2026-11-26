use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr},
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Duration,
};

use crate::env::{Env, LIVE_ENABLED, LiveDisabled};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use nautilus_network::http::{HttpClient, HttpRedirectPolicy, Method, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{Instant, timeout_at},
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("invalid app key")]
    InvalidKey,
    #[error("invalid redirect URI")]
    InvalidRedirect,
    #[error("invalid token response")]
    InvalidTokens,
    #[error("login refused")]
    Refused,
    #[error("login timed out")]
    Timeout,
    #[error("login needed")]
    LoginNeeded,
    #[error("authentication failed")]
    Failed,
}

pub struct AppKey(String);
impl fmt::Debug for AppKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppKey(..)")
    }
}
impl AppKey {
    pub fn from_file(path: &Path) -> Result<Self, AuthError> {
        let file = std::fs::File::open(path).map_err(|_| AuthError::InvalidKey)?;
        if file
            .metadata()
            .map_err(|_| AuthError::InvalidKey)?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(AuthError::InvalidKey);
        }
        let mut value = String::new();
        std::io::Read::read_to_string(&mut &file, &mut value).map_err(|_| AuthError::InvalidKey)?;
        let value = value.trim();
        if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(AuthError::InvalidKey);
        }
        Ok(Self(value.to_owned()))
    }
}

pub struct RedirectUri(String);
impl RedirectUri {
    pub fn new(value: &str) -> Result<Self, AuthError> {
        let url = Url::parse(value).map_err(|_| AuthError::InvalidRedirect)?;
        if url.scheme() != "http"
            || url.host_str() != Some("localhost")
            || url.port().is_none()
            || url.path() == "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.as_str() != value
        {
            return Err(AuthError::InvalidRedirect);
        }
        Ok(Self(value.to_owned()))
    }
}

pub struct Pkce {
    verifier: String,
    challenge: String,
    state: String,
}
impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}
impl Pkce {
    pub fn new() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        Self {
            verifier,
            challenge,
            state,
        }
    }
    pub fn state(&self) -> &str {
        &self.state
    }
    pub fn authorize_url(
        &self,
        env: Env,
        key: &AppKey,
        redirect: &RedirectUri,
    ) -> Result<String, LiveDisabled> {
        let base = auth_base(env)?;
        let mut url = Url::parse(&format!("{base}/authorize")).map_err(|_| LiveDisabled)?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", &key.0),
            ("state", &self.state),
            ("redirect_uri", &redirect.0),
            ("code_challenge", &self.challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(url.into())
    }
}
fn auth_base(env: Env) -> Result<&'static str, LiveDisabled> {
    match env {
        Env::Sim => Ok("https://sim.logonvalidation.net"),
        Env::Live if LIVE_ENABLED => Ok("https://live.logonvalidation.net"),
        Env::Live => Err(LiveDisabled),
    }
}

pub struct Code(String);
impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code(..)")
    }
}

pub async fn wait_for_code(
    listener: TcpListener,
    path: &str,
    state: &str,
    deadline: Instant,
) -> Result<Code, AuthError> {
    let addr = listener.local_addr().map_err(|_| AuthError::Failed)?;
    if addr.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err(AuthError::Failed);
    }
    timeout_at(deadline, async {
        loop {
            let (mut stream, _) = listener.accept().await.map_err(|_| AuthError::Failed)?;
            let mut buf = [0; 8192];
            let mut used = 0;
            let read = tokio::time::timeout(Duration::from_secs(2), async {
                while used < buf.len() && !buf[..used].windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut buf[used..]).await?;
                    if n == 0 {
                        break;
                    }
                    used += n;
                }
                Ok::<_, std::io::Error>(())
            })
            .await;
            match read {
                Err(_) => continue,
                Ok(Err(_)) => continue,
                Ok(Ok(())) => (),
            }
            let request = std::str::from_utf8(&buf[..used]).unwrap_or("");
            let mut parts = request.lines().next().unwrap_or("").split_whitespace();
            let method = parts.next();
            let target = parts.next().unwrap_or("");
            let parsed = Url::parse(&format!("http://localhost{target}")).ok();
            let mut code = None;
            let mut refused = false;
            if method == Some("GET")
                && let Some(url) = parsed
                && url.path() == path
            {
                let pairs: Vec<_> = url.query_pairs().collect();
                let states: Vec<_> = pairs.iter().filter(|(k, _)| k == "state").collect();
                if states.len() == 1 && states[0].1 == state {
                    let codes: Vec<_> = pairs.iter().filter(|(k, _)| k == "code").collect();
                    refused = pairs.iter().any(|(k, _)| k == "error");
                    if !refused && codes.len() == 1 && !codes[0].1.is_empty() {
                        code = Some(Code(codes[0].1.to_string()));
                    }
                }
            }
            let response = if code.is_some() {
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = stream.write_all(response.as_bytes()).await;
            if refused {
                return Err(AuthError::Refused);
            }
            if let Some(code) = code {
                return Ok(code);
            }
        }
    })
    .await
    .map_err(|_| AuthError::Timeout)?
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
    refresh_token_expires_in: u64,
}
pub struct Tokens {
    access: String,
    refresh: String,
    verifier: String,
    access_until: Instant,
    refresh_until: Instant,
}
impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens(..)")
    }
}
impl Tokens {
    fn parse(body: &[u8], verifier: String, now: Instant) -> Result<Self, AuthError> {
        let response: TokenResponse =
            serde_json::from_slice(body).map_err(|_| AuthError::InvalidTokens)?;
        if response.access_token.is_empty()
            || response.refresh_token.is_empty()
            || response.expires_in == 0
            || response.refresh_token_expires_in == 0
        {
            return Err(AuthError::InvalidTokens);
        }
        Ok(Self {
            access: response.access_token,
            refresh: response.refresh_token,
            verifier,
            access_until: now
                .checked_add(Duration::from_secs(response.expires_in))
                .ok_or(AuthError::InvalidTokens)?,
            refresh_until: now
                .checked_add(Duration::from_secs(response.refresh_token_expires_in))
                .ok_or(AuthError::InvalidTokens)?,
        })
    }
}
pub struct TokenManager {
    client: HttpClient,
    base: String,
    tokens: Tokens,
    failed: bool,
}
impl TokenManager {
    #[cfg(test)]
    pub(crate) fn test_token() -> Self {
        let now = Instant::now();
        Self {
            client: Self::client().expect("client"),
            base: "http://127.0.0.1".to_owned(),
            tokens: Tokens {
                access: "test-token".to_owned(),
                refresh: "refresh".to_owned(),
                verifier: "verifier".to_owned(),
                access_until: now + Duration::from_secs(3600),
                refresh_until: now + Duration::from_secs(3600),
            },
            failed: false,
        }
    }
    fn client() -> Result<HttpClient, AuthError> {
        HttpClient::builder()
            .use_system_proxy(false)
            .redirect_policy(HttpRedirectPolicy::Reject)
            .timeout_secs(20)
            .redact_url(true)
            .build()
            .map_err(|_| AuthError::Failed)
    }
    pub async fn exchange(
        env: Env,
        key: &AppKey,
        pkce: Pkce,
        redirect: &RedirectUri,
        code: Code,
    ) -> Result<Self, AuthError> {
        let base = auth_base(env).map_err(|_| AuthError::Failed)?.to_owned();
        Self::exchange_at(base, key, pkce, redirect, code).await
    }
    async fn exchange_at(
        base: String,
        key: &AppKey,
        pkce: Pkce,
        redirect: &RedirectUri,
        code: Code,
    ) -> Result<Self, AuthError> {
        let client = Self::client()?;
        let body = form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &key.0),
            ("code", &code.0),
            ("redirect_uri", &redirect.0),
            ("code_verifier", &pkce.verifier),
        ]);
        let response = post(&client, &base, body).await?;
        let tokens = Tokens::parse(&response, pkce.verifier, Instant::now())?;
        Ok(Self {
            client,
            base,
            tokens,
            failed: false,
        })
    }
    pub async fn access(&mut self, now: Instant) -> Result<&str, AuthError> {
        if self.failed || now >= self.tokens.refresh_until {
            self.failed = true;
            return Err(AuthError::LoginNeeded);
        }
        if self.tokens.access_until.saturating_duration_since(now) < Duration::from_secs(120) {
            let body = form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.tokens.refresh),
                ("code_verifier", &self.tokens.verifier),
            ]);
            let result = post(&self.client, &self.base, body).await.and_then(|body| {
                Tokens::parse(&body, self.tokens.verifier.clone(), Instant::now())
            });
            match result {
                Ok(tokens) => self.tokens = tokens,
                Err(_) => {
                    self.failed = true;
                    return Err(AuthError::LoginNeeded);
                }
            }
        }
        Ok(&self.tokens.access)
    }
}
fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut url = Url::parse("http://localhost/").expect("static URL");
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    url.query().unwrap_or("").as_bytes().to_vec()
}
async fn post(client: &HttpClient, base: &str, body: Vec<u8>) -> Result<bytes::Bytes, AuthError> {
    let headers = HashMap::from([(
        "content-type".to_owned(),
        "application/x-www-form-urlencoded".to_owned(),
    )]);
    let response = client
        .request(
            Method::POST,
            format!("{base}/token"),
            None,
            Some(headers),
            Some(body),
            None,
            None,
        )
        .await
        .map_err(|_| AuthError::Failed)?;
    if !response.status.is_success() {
        return Err(AuthError::Failed);
    }
    Ok(response.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::TcpStream;

    async fn callback(addr: std::net::SocketAddr, target: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .await
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).await.expect("read");
        response
    }
    #[tokio::test]
    async fn idle_preconnection_does_not_block_callback() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let waiter = tokio::spawn(wait_for_code(
            listener,
            "/cb",
            "right",
            Instant::now() + Duration::from_secs(10),
        ));
        let _idle = TcpStream::connect(addr).await.expect("preconnect");
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            callback(addr, "/cb?state=right&code=secret")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(waiter.await.expect("task").expect("code").0, "secret");
    }
    #[tokio::test]
    async fn callback_checks_path_state_refusal_and_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let waiter = tokio::spawn(wait_for_code(
            listener,
            "/cb",
            "right",
            Instant::now() + Duration::from_secs(5),
        ));
        assert!(
            callback(addr, "/cb?state=wrong&code=secret")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(
            callback(addr, "/wrong?state=right&code=secret")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(
            callback(addr, "/cb?state=right&code=secret")
                .await
                .starts_with("HTTP/1.1 200")
        );
        assert_eq!(waiter.await.expect("task").expect("code").0, "secret");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let waiter = tokio::spawn(wait_for_code(
            listener,
            "/cb",
            "right",
            Instant::now() + Duration::from_secs(5),
        ));
        assert!(
            callback(addr, "/cb?state=right&error=access_denied")
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(matches!(
            waiter.await.expect("task"),
            Err(AuthError::Refused)
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        assert!(matches!(
            wait_for_code(
                listener,
                "/cb",
                "right",
                Instant::now() + Duration::from_millis(10)
            )
            .await,
            Err(AuthError::Timeout)
        ));
    }
    async fn server(
        responses: Vec<&'static str>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let task = tokio::spawn(async move {
            let mut requests = Vec::new();
            for body in responses {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                loop {
                    let mut part = [0; 4096];
                    let n = stream.read(&mut part).await.expect("read");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&part[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&buf[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|v| v.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8(buf).expect("utf8"));
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.expect("write");
            }
            requests
        });
        (format!("http://{addr}"), task)
    }
    const VALID: &str = r#"{"access_token":"old_access","refresh_token":"old_refresh","expires_in":60,"refresh_token_expires_in":3600}"#;
    const NEW: &str = r#"{"access_token":"new_access","refresh_token":"new_refresh","expires_in":3600,"refresh_token_expires_in":3600}"#;
    #[tokio::test]
    async fn tokens_refresh_and_fail_closed() {
        assert!(
            Tokens::parse(
                br#"{"access_token":"a","refresh_token":"b","refresh_token_expires_in":3600}"#,
                "verifier".into(),
                Instant::now()
            )
            .is_err()
        );
        let (base, task) = server(vec![VALID, NEW]).await;
        let key = AppKey("12345678901234567890123456789012".into());
        let pkce = Pkce::new();
        let verifier = pkce.verifier.clone();
        let redirect = RedirectUri::new("http://localhost:1234/cb").expect("redirect");
        let mut manager =
            TokenManager::exchange_at(base, &key, pkce, &redirect, Code("code".into()))
                .await
                .expect("exchange");
        assert_eq!(
            manager.access(Instant::now()).await.expect("refresh"),
            "new_access"
        );
        assert_eq!(manager.tokens.refresh, "new_refresh");
        let requests = task.await.expect("task");
        assert!(requests[1].contains("grant_type=refresh_token"));
        assert!(requests[1].contains("refresh_token=old_refresh"));
        assert!(requests[1].contains(&format!("code_verifier={verifier}")));
        assert!(!format!("{:?}", manager.tokens).contains("new_access"));
        assert!(!format!("{key:?}").contains(&key.0));
        let (base, task) = server(vec!["{}"]).await;
        let mut manager = TokenManager {
            client: TokenManager::client().expect("client"),
            base,
            tokens: Tokens::parse(VALID.as_bytes(), "verifier".into(), Instant::now())
                .expect("tokens"),
            failed: false,
        };
        assert_eq!(
            manager.access(Instant::now()).await,
            Err(AuthError::LoginNeeded)
        );
        assert_eq!(
            manager.access(Instant::now()).await,
            Err(AuthError::LoginNeeded)
        );
        assert_eq!(task.await.expect("task").len(), 1);
    }
    #[test]
    fn live_and_key_permissions() {
        let file = tempfile::NamedTempFile::new().expect("file");
        std::fs::write(file.path(), "12345678901234567890123456789012\n").expect("write");
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o644))
            .expect("chmod");
        assert!(AppKey::from_file(file.path()).is_err());
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        let key = AppKey::from_file(file.path()).expect("key");
        let redirect = RedirectUri::new("http://localhost:1234/cb").expect("redirect");
        assert!(
            Pkce::new()
                .authorize_url(Env::Live, &key, &redirect)
                .is_err()
        );
        assert!(Env::Live.base_url().is_err());
    }
}

use crate::{auth::TokenManager, env::Env};
use nautilus_common::factories::ClientConfig;
use std::{any::Any, fmt, sync::Arc};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct SaxoExecutionClientConfig {
    pub env: Env,
    pub account_key: String,
    pub poll_interval: std::time::Duration,
    pub tokens: Arc<Mutex<TokenManager>>,
}
impl SaxoExecutionClientConfig {
    pub fn new(env: Env, account_key: String, tokens: Arc<Mutex<TokenManager>>) -> Self {
        Self {
            env,
            account_key,
            tokens,
            poll_interval: std::time::Duration::from_secs(1),
        }
    }
}
#[derive(Clone)]
pub struct SaxoDataClientConfig {
    pub poll_interval: std::time::Duration,
    pub amount: u64,
    pub env: Env,
    pub tokens: Arc<Mutex<TokenManager>>,
}
impl SaxoDataClientConfig {
    pub fn new(env: Env, tokens: Arc<Mutex<TokenManager>>) -> Self {
        Self {
            env,
            tokens,
            poll_interval: std::time::Duration::from_secs(1),
            amount: 5000,
        }
    }
}
impl fmt::Debug for SaxoExecutionClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaxoExecutionClientConfig")
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for SaxoDataClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SaxoDataClientConfig")
            .field("env", &self.env)
            .finish_non_exhaustive()
    }
}
impl ClientConfig for SaxoExecutionClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl ClientConfig for SaxoDataClientConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_hides_secrets() {
        let tokens = Arc::new(Mutex::new(TokenManager::test_token()));
        let execution = SaxoExecutionClientConfig {
            env: Env::Sim,
            account_key: "secret-key".into(),
            poll_interval: std::time::Duration::from_secs(1),
            tokens: tokens.clone(),
        };
        let data = SaxoDataClientConfig::new(Env::Sim, tokens);
        assert!(!format!("{execution:?} {data:?}").contains("secret-key"));
        assert!(!format!("{execution:?} {data:?}").contains("test-token"));
    }
}

use crate::{
    config::{SaxoDataClientConfig, SaxoExecutionClientConfig},
    data::SaxoDataClient,
    execution::SaxoExecutionClient,
};
use nautilus_common::{
    cache::CacheView,
    clients::{DataClient, ExecutionClient},
    clock::Clock,
    factories::{ClientConfig, DataClientFactory, ExecutionClientFactory},
};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{AccountType, OmsType},
    identifiers::{AccountId, ClientId, TraderId, Venue},
};
use std::{cell::RefCell, rc::Rc};

#[derive(Debug, Default, Clone)]
pub struct SaxoDataClientFactory;
impl DataClientFactory for SaxoDataClientFactory {
    fn create(
        &self,
        name: &str,
        config: &dyn ClientConfig,
        _cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn DataClient>> {
        let config = config
            .as_any()
            .downcast_ref::<SaxoDataClientConfig>()
            .ok_or_else(|| anyhow::anyhow!("expected SaxoDataClientConfig"))?
            .clone();
        Ok(Box::new(SaxoDataClient::new(
            ClientId::new_checked(name)?,
            config,
        )?))
    }
    fn name(&self) -> &str {
        "SAXO"
    }
    fn config_type(&self) -> &str {
        "SaxoDataClientConfig"
    }
}
#[derive(Debug, Default, Clone)]
pub struct SaxoExecutionClientFactory;
impl ExecutionClientFactory for SaxoExecutionClientFactory {
    fn create(
        &self,
        trader_id: TraderId,
        name: &str,
        config: &dyn ClientConfig,
        cache: CacheView,
        _clock: Rc<RefCell<dyn Clock>>,
    ) -> anyhow::Result<Box<dyn ExecutionClient>> {
        let config = config
            .as_any()
            .downcast_ref::<SaxoExecutionClientConfig>()
            .ok_or_else(|| anyhow::anyhow!("expected SaxoExecutionClientConfig"))?
            .clone();
        let core = ExecutionClientCore::new(
            trader_id,
            ClientId::new_checked(name)?,
            Venue::from("SAXO"),
            OmsType::Netting,
            AccountId::from("SAXO-001"),
            AccountType::Margin,
            None,
            cache,
        );
        Ok(Box::new(SaxoExecutionClient::new(core, config)?))
    }
    fn name(&self) -> &str {
        "SAXO"
    }
    fn config_type(&self) -> &str {
        "SaxoExecutionClientConfig"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::TokenManager, env::Env};
    use nautilus_common::{cache::Cache, live::clock::LiveClock};
    use std::sync::Arc;
    use tokio::sync::Mutex;
    #[test]
    fn factories_reject_wrong_config_type() {
        let tokens = Arc::new(Mutex::new(TokenManager::test_token()));
        let data = SaxoDataClientConfig::new(Env::Sim, tokens.clone());
        let execution = SaxoExecutionClientConfig::new(Env::Sim, "key".into(), tokens);
        let cache = Rc::new(RefCell::new(Cache::default()));
        let clock: Rc<RefCell<dyn Clock>> = Rc::new(RefCell::new(LiveClock::new(None)));
        assert!(
            SaxoDataClientFactory
                .create("SAXO", &execution, cache.clone().into(), clock.clone())
                .is_err()
        );
        assert!(
            SaxoExecutionClientFactory
                .create(
                    TraderId::from("TESTER-001"),
                    "SAXO",
                    &data,
                    cache.into(),
                    clock
                )
                .is_err()
        );
    }
}

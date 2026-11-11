use std::{collections::HashSet, time::Duration};

use anyhow::{Context, ensure};
use futures::StreamExt;
use ibapi::{
    accounts::{
        AccountSummaryResult, PositionUpdate,
        types::{AccountGroup, AccountId},
    },
    contracts::SecurityType,
    orders::Orders,
    prelude::SubscriptionItemStreamExt,
};
use nautilus_common::{enums::Environment, logging::logger::LoggerConfig};
use nautilus_interactive_brokers::{
    common::shared_client::{self, SharedClientHandle},
    config::{
        InteractiveBrokersDataClientConfig, InteractiveBrokersExecutionClientConfig,
        InteractiveBrokersInstrumentProviderConfig, MarketDataType,
    },
    factories::{InteractiveBrokersDataClientFactory, InteractiveBrokersExecutionClientFactory},
};
use nautilus_live::{config::RoutingConfig, node::LiveNode};
use nautilus_model::identifiers::{InstrumentId, TraderId};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 7497;
const CLIENT_ID: i32 = 8;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Snapshot {
    pub position: i64,
    pub open_orders: usize,
    pub daily_pnl_usd: Option<f64>,
}

pub async fn build_node() -> anyhow::Result<(LiveNode, SharedClientHandle)> {
    let shared = shared_client::get_or_connect(HOST, PORT, CLIENT_ID, 10, Some(0))
        .await
        .map_err(|_| anyhow::anyhow!("failed to connect to TWS"))?;
    let accounts = tokio::time::timeout(REQUEST_TIMEOUT, shared.managed_accounts())
        .await
        .map_err(|_| anyhow::anyhow!("timed out reading managed accounts"))?
        .map_err(|_| anyhow::anyhow!("failed to read managed accounts"))?;
    let account = crate::cli::paper_account(&accounts).map_err(anyhow::Error::msg)?;

    let instrument_id = InstrumentId::from("SPY=STK.SMART");
    let mut load_ids = HashSet::new();
    load_ids.insert(instrument_id);
    let provider = InteractiveBrokersInstrumentProviderConfig {
        load_ids,
        ..Default::default()
    };
    let data_config = InteractiveBrokersDataClientConfig {
        host: HOST.to_owned(),
        port: PORT,
        client_id: CLIENT_ID,
        market_data_type: MarketDataType::Delayed,
        instrument_provider: provider.clone(),
        request_timeout: REQUEST_TIMEOUT.as_secs(),
        ..Default::default()
    };
    let exec_config = InteractiveBrokersExecutionClientConfig {
        host: HOST.to_owned(),
        port: PORT,
        client_id: CLIENT_ID,
        account_id: Some(account.to_owned()),
        instrument_provider: provider,
        request_timeout: REQUEST_TIMEOUT.as_secs(),
        ..Default::default()
    };
    let routing = RoutingConfig::builder().default(true).build();
    let node = LiveNode::builder(TraderId::from("PAPER-ORDER"), Environment::Live)?
        .with_name("PAPER-ORDER".to_owned())
        .with_reconciliation(true)
        .with_logging(LoggerConfig::from_spec(
            "stdout=Off;fileout=Off;bypass_logging",
        )?)
        .add_data_client_with_routing(
            None,
            Box::new(InteractiveBrokersDataClientFactory::new()),
            Box::new(data_config),
            routing.clone(),
        )?
        .add_exec_client_with_routing(
            None,
            Box::new(InteractiveBrokersExecutionClientFactory::new()),
            Box::new(exec_config),
            routing,
        )?
        .build()?;

    Ok((node, shared))
}

pub async fn snapshot(shared: &SharedClientHandle) -> anyhow::Result<Snapshot> {
    let accounts = tokio::time::timeout(REQUEST_TIMEOUT, shared.managed_accounts())
        .await
        .map_err(|_| anyhow::anyhow!("timed out verifying managed accounts"))?
        .map_err(|_| anyhow::anyhow!("failed to verify managed accounts"))?;
    let account = crate::cli::paper_account(&accounts).map_err(anyhow::Error::msg)?;
    let position = read_position(shared, account).await?;
    let open_orders = read_open_orders(shared, account).await?;
    let daily_pnl_usd = read_daily_pnl(shared, account).await?;
    Ok(Snapshot {
        position,
        open_orders,
        daily_pnl_usd,
    })
}

async fn read_position(shared: &SharedClientHandle, account: &str) -> anyhow::Result<i64> {
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut positions = shared
            .positions()
            .await
            .map_err(|_| anyhow::anyhow!("failed to request positions"))?
            .filter_data();
        let mut total = 0_i64;
        while let Some(item) = positions.next().await {
            match item.map_err(|_| anyhow::anyhow!("failed to read positions"))? {
                PositionUpdate::Position(position)
                    if position.account == account
                        && position.contract.symbol.as_str() == "SPY"
                        && position.contract.security_type == SecurityType::Stock
                        && position.contract.currency.as_str() == "USD" =>
                {
                    let shares = shares_from_f64(position.position)?;
                    total = total.checked_add(shares).context("SPY position overflow")?;
                }
                PositionUpdate::PositionEnd => return Ok(total),
                PositionUpdate::Position(_) => {}
            }
        }
        anyhow::bail!("position stream ended before PositionEnd")
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading positions"))??;
    Ok(result)
}

async fn read_open_orders(shared: &SharedClientHandle, account: &str) -> anyhow::Result<usize> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut orders = shared
            .all_open_orders()
            .await
            .map_err(|_| anyhow::anyhow!("failed to request open orders"))?
            .filter_data();
        let mut count = 0_usize;
        while let Some(item) = orders.next().await {
            if let Orders::OrderData(data) =
                item.map_err(|_| anyhow::anyhow!("failed to read open orders"))?
                && data.order.account == account
                && data.contract.symbol.as_str() == "SPY"
                && data.contract.security_type == SecurityType::Stock
                && data.contract.currency.as_str() == "USD"
            {
                count = count.checked_add(1).context("open-order count overflow")?;
            }
        }
        Ok::<_, anyhow::Error>(count)
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading open orders"))?
}

async fn read_daily_pnl(shared: &SharedClientHandle, account: &str) -> anyhow::Result<Option<f64>> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut summary = shared
            .account_summary(
                &AccountGroup::from("All"),
                &[ibapi::accounts::AccountSummaryTags::NET_LIQUIDATION],
            )
            .await
            .map_err(|_| anyhow::anyhow!("failed to verify account currency"))?
            .filter_data();
        let mut usd_base = false;
        let mut matching_rows = 0;
        while let Some(item) = summary.next().await {
            match item.map_err(|_| anyhow::anyhow!("failed to read account currency"))? {
                AccountSummaryResult::Summary(row)
                    if row.account == account
                        && row.tag == ibapi::accounts::AccountSummaryTags::NET_LIQUIDATION =>
                {
                    matching_rows += 1;
                    usd_base = row.currency == "USD";
                }
                AccountSummaryResult::Summary(_) => {}
                AccountSummaryResult::End => break,
            }
        }
        if matching_rows != 1 || !usd_base {
            return Ok(None);
        }

        let mut pnl = shared
            .pnl(&AccountId::from(account), None)
            .await
            .map_err(|_| anyhow::anyhow!("failed to subscribe to daily P&L"))?
            .filter_data();
        let value = pnl
            .next()
            .await
            .transpose()
            .map_err(|_| anyhow::anyhow!("failed to read daily P&L"))?;
        Ok(value
            .map(|value| value.daily_pnl)
            .filter(|value| value.is_finite()))
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading daily P&L"))?
}

fn shares_from_f64(position: f64) -> anyhow::Result<i64> {
    ensure!(
        position.is_finite()
            && position.fract() == 0.0
            && position >= i64::MIN as f64
            && position < -(i64::MIN as f64),
        "SPY position is not an in-range integer share count"
    );
    Ok(position as i64)
}

#[cfg(test)]
mod tests {
    use super::shares_from_f64;

    #[test]
    fn share_conversion_rejects_float_rounding_at_integer_bounds() {
        assert_eq!(shares_from_f64(i64::MIN as f64).ok(), Some(i64::MIN));
        assert!(shares_from_f64(-(i64::MIN as f64)).is_err());
        assert!(shares_from_f64(i64::MAX as f64).is_err());
        assert!(shares_from_f64(1.5).is_err());
    }
}

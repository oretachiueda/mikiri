use std::{collections::HashSet, path::Path, time::Duration};

use anyhow::{Context, ensure};
use futures::StreamExt;
use ibapi::{
    accounts::{
        AccountSummary, AccountSummaryResult, AccountSummaryTags, PositionUpdate,
        types::AccountGroup,
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
        InteractiveBrokersInstrumentProviderConfig, MarketDataType, SymbologyMethod,
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
    pub current_equity_usd: anyhow::Result<f64>,
}

impl Snapshot {
    pub fn daily_pnl(
        &self,
        path: &Path,
        today: time::Date,
        journal: &guard::Journal,
        account: &str,
    ) -> Option<f64> {
        let result = match &self.current_equity_usd {
            Ok(equity) => crate::baseline::pnl(path, today, *equity, journal),
            Err(error) => Err(anyhow::anyhow!("{error:#}")),
        };
        match result {
            Ok(pnl) => Some(pnl),
            Err(error) => {
                eprintln!(
                    "{}",
                    crate::errors::mask(&format!("{error:#}"), Some(account))
                );
                None
            }
        }
    }

    pub fn status_line(&self, journal_net: i64, daily_pnl_usd: Option<f64>) -> String {
        let pnl = daily_pnl_usd.map_or_else(|| "unknown".into(), |value| format!("{value:.2}"));
        format!(
            "status account=paper position={} journal_net={journal_net} open_orders={} match={} daily_pnl_usd={pnl}",
            self.position,
            self.open_orders,
            self.position == journal_net
        )
    }
}

pub async fn connect() -> anyhow::Result<(SharedClientHandle, String)> {
    let shared = shared_client::get_or_connect(HOST, PORT, CLIENT_ID, 10, Some(0))
        .await
        .context("failed to connect to TWS")?;
    let accounts = tokio::time::timeout(REQUEST_TIMEOUT, shared.managed_accounts())
        .await
        .map_err(|_| anyhow::anyhow!("timed out reading managed accounts"))?
        .context("failed to read managed accounts")?;
    let account = crate::cli::paper_account(&accounts).map_err(anyhow::Error::msg)?;

    Ok((shared, account.to_owned()))
}

pub async fn require_writable(shared: &SharedClientHandle) -> anyhow::Result<()> {
    // The execution engine discards connect errors; preserve TWS 321 before startup.
    tokio::time::timeout(REQUEST_TIMEOUT, shared.next_valid_order_id())
        .await
        .context("timed out checking TWS API mode")?
        .context("cannot initialize execution order IDs")?;
    Ok(())
}

pub fn build_node(account: &str) -> anyhow::Result<LiveNode> {
    let provider = instrument_provider();
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

    Ok(node)
}

fn instrument_provider() -> InteractiveBrokersInstrumentProviderConfig {
    InteractiveBrokersInstrumentProviderConfig {
        load_ids: HashSet::from([InstrumentId::from("SPY=STK.SMART")]),
        symbology_method: SymbologyMethod::Raw,
        ..Default::default()
    }
}

pub async fn snapshot(shared: &SharedClientHandle) -> anyhow::Result<Snapshot> {
    let accounts = tokio::time::timeout(REQUEST_TIMEOUT, shared.managed_accounts())
        .await
        .map_err(|_| anyhow::anyhow!("timed out verifying managed accounts"))?
        .context("failed to verify managed accounts")?;
    let account = crate::cli::paper_account(&accounts).map_err(anyhow::Error::msg)?;
    let position = read_position(shared, account).await?;
    let open_orders = read_open_orders(shared, account).await?;
    let current_equity_usd = read_current_equity(shared, account).await;
    Ok(Snapshot {
        position,
        open_orders,
        current_equity_usd,
    })
}

async fn read_position(shared: &SharedClientHandle, account: &str) -> anyhow::Result<i64> {
    let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut positions = shared
            .positions()
            .await
            .context("failed to request positions")?
            .filter_data();
        let mut total = 0_i64;
        while let Some(item) = positions.next().await {
            match item.context("failed to read positions")? {
                PositionUpdate::Position(position) => {
                    if let Some(shares) = position_row_shares(
                        position.contract.symbol.as_str(),
                        &position.contract.security_type,
                        position.contract.currency.as_str(),
                        &position.account,
                        account,
                        position.position,
                    )? {
                        total = total.checked_add(shares).context("SPY position overflow")?;
                    }
                }
                PositionUpdate::PositionEnd => return Ok(total),
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
            .context("failed to request open orders")?
            .filter_data();
        let mut count = 0_usize;
        while let Some(item) = orders.next().await {
            if let Orders::OrderData(data) = item.context("failed to read open orders")?
                && open_order_row_matches(
                    data.contract.symbol.as_str(),
                    &data.contract.security_type,
                    data.contract.currency.as_str(),
                    &data.order.account,
                    account,
                )?
            {
                count = count.checked_add(1).context("open-order count overflow")?;
            }
        }
        finish_open_orders(count, shared.is_connected())
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading open orders"))?
}

#[derive(Default)]
struct CurrentEquity {
    current: Option<f64>,
    invalid: Option<&'static str>,
}

impl CurrentEquity {
    fn push(&mut self, row: &AccountSummary, account: &str) {
        if row.account != account {
            return;
        }
        let field = match row.tag.as_str() {
            AccountSummaryTags::EQUITY_WITH_LOAN_VALUE => &mut self.current,
            _ => return,
        };
        let value = row
            .value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite());
        if let (Some(first), Some(next)) = (*field, value)
            && first.to_bits() != next.to_bits()
        {
            self.invalid.get_or_insert("current equity values disagree");
        }
        if row.currency != "USD" {
            self.invalid.get_or_insert("current equity is not USD");
        }
        if value.is_none() {
            self.invalid
                .get_or_insert("current equity is not finite numeric data");
        }
        if field.is_none() {
            *field = value;
        }
    }

    fn finish(self) -> anyhow::Result<f64> {
        if let Some(reason) = self.invalid {
            anyhow::bail!(reason);
        }
        self.current
            .ok_or_else(|| anyhow::anyhow!("current equity is missing"))
    }
}

async fn read_current_equity(shared: &SharedClientHandle, account: &str) -> anyhow::Result<f64> {
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut summary = shared
            .account_summary(
                &AccountGroup::from("All"),
                &[AccountSummaryTags::EQUITY_WITH_LOAN_VALUE],
            )
            .await
            .map_err(|_| anyhow::anyhow!("failed to request account equity summary"))?
            .filter_data();
        let mut equity = CurrentEquity::default();
        while let Some(item) = summary.next().await {
            match item.map_err(|_| anyhow::anyhow!("failed to read account equity summary"))? {
                AccountSummaryResult::Summary(row) => equity.push(&row, account),
                AccountSummaryResult::End => return equity.finish(),
            }
        }
        anyhow::bail!("account equity summary ended before snapshot completion")
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out reading current equity"))?
}

fn position_row_shares(
    symbol: &str,
    security_type: &SecurityType,
    currency: &str,
    actual_account: &str,
    verified_account: &str,
    quantity: f64,
) -> anyhow::Result<Option<i64>> {
    if !is_spy_stock_usd(symbol, security_type, currency) {
        return Ok(None);
    }
    require_verified_account(actual_account, verified_account)?;
    shares_from_f64(quantity).map(Some)
}

fn open_order_row_matches(
    symbol: &str,
    security_type: &SecurityType,
    currency: &str,
    actual_account: &str,
    verified_account: &str,
) -> anyhow::Result<bool> {
    if !is_spy_stock_usd(symbol, security_type, currency) {
        return Ok(false);
    }
    require_verified_account(actual_account, verified_account)?;
    Ok(true)
}

fn is_spy_stock_usd(symbol: &str, security_type: &SecurityType, currency: &str) -> bool {
    symbol == "SPY" && security_type == &SecurityType::Stock && currency == "USD"
}

fn require_verified_account(actual: &str, verified: &str) -> anyhow::Result<()> {
    ensure!(actual == verified, "SPY account identity mismatch");
    Ok(())
}

fn finish_open_orders(count: usize, connected: bool) -> anyhow::Result<usize> {
    ensure!(connected, "IB disconnected while reading open orders");
    Ok(count)
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
    use super::*;

    fn equity_row(value: &str, currency: &str, account: &str) -> AccountSummary {
        AccountSummary {
            account: account.into(),
            tag: AccountSummaryTags::EQUITY_WITH_LOAN_VALUE.into(),
            value: value.into(),
            currency: currency.into(),
        }
    }

    #[test]
    fn current_equity_requires_consistent_finite_usd_data_for_verified_account() {
        let mut equity = CurrentEquity::default();
        equity.push(&equity_row("100", "USD", "DU123"), "DU123");
        equity.push(&equity_row("200", "USD", "DU999"), "DU123");
        assert_eq!(equity.finish().unwrap(), 100.0);
        for rows in [
            vec![],
            vec![equity_row("100", "USD", "DU999")],
            vec![equity_row("100", "USD", "")],
            vec![
                equity_row("100", "USD", "DU123"),
                equity_row("101", "USD", "DU123"),
            ],
            vec![equity_row("100", "JPY", "DU123")],
            vec![equity_row("100", "", "DU123")],
            vec![equity_row("NaN", "USD", "DU123")],
            vec![equity_row("inf", "USD", "DU123")],
            vec![equity_row("invalid", "USD", "DU123")],
        ] {
            let mut equity = CurrentEquity::default();
            for row in rows {
                equity.push(&row, "DU123");
            }
            assert!(equity.finish().is_err());
        }
    }

    #[test]
    fn single_and_bit_identical_equity_rows_are_accepted() {
        for values in [
            vec!["100"],
            vec!["100", "100"],
            vec!["100.0", "100.00", "1e2"],
        ] {
            let mut equity = CurrentEquity::default();
            for value in values {
                equity.push(&equity_row(value, "USD", "DU123"), "DU123");
            }
            assert_eq!(equity.finish().unwrap().to_bits(), 100.0_f64.to_bits());
        }
    }

    #[test]
    fn differing_equity_rows_stay_unknown_even_if_first_value_returns() {
        for values in [
            vec!["100", "101"],
            vec!["100", "101", "100"],
            vec!["0", "-0"],
        ] {
            let mut equity = CurrentEquity::default();
            for value in values {
                equity.push(&equity_row(value, "USD", "DU123"), "DU123");
            }
            assert_eq!(
                equity.finish().unwrap_err().to_string(),
                "current equity values disagree"
            );
        }
        let adjacent = f64::from_bits(100.0_f64.to_bits() + 1).to_string();
        let mut equity = CurrentEquity::default();
        for value in ["100", adjacent.as_str()] {
            equity.push(&equity_row(value, "USD", "DU123"), "DU123");
        }
        assert!(equity.finish().is_err());
    }

    #[test]
    fn invalid_or_missing_current_equity_never_creates_baseline_and_blocks_orders() {
        let directory =
            std::env::temp_dir().join(format!("mikiri-equity-{}", nautilus_core::UUID4::new()));
        std::fs::create_dir(&directory).unwrap();
        let journal = guard::Journal::open(directory.join("journal")).unwrap();
        for reason in [
            "current equity is missing",
            "current equity values disagree",
            "current equity is not USD",
            "current equity is not finite numeric data",
        ] {
            let snapshot = Snapshot {
                position: 0,
                open_orders: 0,
                current_equity_usd: Err(anyhow::anyhow!(reason)),
            };
            let pnl = snapshot.daily_pnl(
                &directory.join("baseline"),
                time::macros::date!(2026 - 11 - 12),
                &journal,
                "DU123",
            );
            assert!(pnl.is_none());
            assert!(!directory.join("baseline").exists());
            assert!(crate::cli::gate(0, 0, 0, pnl).is_err());
            assert_eq!(
                snapshot.status_line(0, pnl),
                "status account=paper position=0 journal_net=0 open_orders=0 match=true daily_pnl_usd=unknown"
            );
        }
        drop(journal);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn preload_ids_match_provider_symbology() {
        use ibapi::contracts::Contract;
        use nautilus_interactive_brokers::common::symbology::Symbology;
        use nautilus_model::identifiers::Venue;

        let config = instrument_provider();
        let contract = Contract::stock("SPY").build();
        let id = Symbology::new(config.symbology_method)
            .instrument_id(&contract, Some(Venue::from("SMART")))
            .unwrap();
        assert_eq!(id, InstrumentId::from("SPY=STK.SMART"));
        assert!(config.load_ids.contains(&id));
        let simplified = Symbology::new(SymbologyMethod::Simplified)
            .instrument_id(&contract, Some(Venue::from("SMART")))
            .unwrap();
        assert_ne!(id, simplified);
    }

    #[test]
    fn share_conversion_rejects_float_rounding_at_integer_bounds() {
        assert_eq!(shares_from_f64(i64::MIN as f64).ok(), Some(i64::MIN));
        assert!(shares_from_f64(-(i64::MIN as f64)).is_err());
        assert!(shares_from_f64(i64::MAX as f64).is_err());
        assert!(shares_from_f64(1.5).is_err());
    }

    #[test]
    fn target_positions_reject_mismatched_or_blank_account() {
        for account in ["DU999", ""] {
            assert!(
                position_row_shares("SPY", &SecurityType::Stock, "USD", account, "DU123", 1.0)
                    .is_err()
            );
        }
        assert_eq!(
            position_row_shares("SPY", &SecurityType::Stock, "USD", "DU123", "DU123", 1.0).unwrap(),
            Some(1)
        );
        assert_eq!(
            position_row_shares("QQQ", &SecurityType::Stock, "USD", "", "DU123", 1.0).unwrap(),
            None
        );
    }

    #[test]
    fn target_open_orders_reject_mismatched_or_blank_account() {
        for account in ["DU999", ""] {
            assert!(
                open_order_row_matches("SPY", &SecurityType::Stock, "USD", account, "DU123")
                    .is_err()
            );
        }
        assert!(
            open_order_row_matches("SPY", &SecurityType::Stock, "USD", "DU123", "DU123").unwrap()
        );
        assert!(!open_order_row_matches("QQQ", &SecurityType::Stock, "USD", "", "DU123").unwrap());
    }

    #[test]
    fn disconnected_open_order_stream_end_is_an_error_even_when_empty() {
        assert!(finish_open_orders(0, false).is_err());
        assert_eq!(finish_open_orders(0, true).unwrap(), 0);
    }
}

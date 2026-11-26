use anyhow::{Context, ensure};
use nautilus_core::{Params, UnixNanos};
use nautilus_model::{
    enums::{LiquiditySide, OrderSide, OrderStatus, OrderType, PositionSide, TimeInForce},
    identifiers::{AccountId, ClientOrderId, InstrumentId, TradeId, VenueOrderId},
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, Currency, MarginBalance, Money, Price, Quantity},
};
use serde_json::Value;

fn text<'a>(row: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {key}"))
}
fn number(row: &Value, key: &str) -> anyhow::Result<f64> {
    row.get(key)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .with_context(|| format!("missing or invalid {key}"))
}
fn qty(value: f64) -> anyhow::Result<Quantity> {
    ensure!(value >= 0.0 && value.fract() == 0.0, "invalid quantity");
    Ok(Quantity::new_checked(value, 0)?)
}
fn timestamp(s: &str) -> anyhow::Result<UnixNanos> {
    let dt = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)?;
    Ok(u64::try_from(dt.unix_timestamp_nanos())?.into())
}
fn instrument() -> InstrumentId {
    InstrumentId::from("USD/JPY.SAXO")
}
fn side(row: &Value) -> anyhow::Result<OrderSide> {
    match text(row, "BuySell")? {
        "Buy" => Ok(OrderSide::Buy),
        "Sell" => Ok(OrderSide::Sell),
        _ => anyhow::bail!("unsupported side"),
    }
}
fn client(row: &Value) -> Option<ClientOrderId> {
    row.get("ExternalReference")
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 50
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .map(ClientOrderId::from)
}
fn rows(value: &Value) -> anyhow::Result<&Vec<Value>> {
    value
        .get("Data")
        .and_then(Value::as_array)
        .context("missing Data")
}
fn supported(row: &Value) -> bool {
    row["Uic"].as_u64() == Some(42) && row["AssetType"].as_str() == Some("FxSpot")
}

pub(crate) fn positions(
    value: &Value,
    account: AccountId,
    now: UnixNanos,
) -> anyhow::Result<PositionStatusReport> {
    let mut count = 0;
    let mut force = false;
    let mut net = 0.0;
    for row in rows(value)? {
        let base = row.get("PositionBase").context("missing PositionBase")?;
        if !supported(base) {
            continue;
        }
        count += 1;
        force |= base
            .get("IsForceOpen")
            .and_then(Value::as_bool)
            .context("missing IsForceOpen")?;
        net += number(base, "Amount")?;
    }
    ensure!(
        count < 2 || !force,
        "multiple force-open positions require manual reconciliation"
    );
    ensure!(net.is_finite(), "invalid net position");
    let direction = if net > 0.0 {
        PositionSide::Long
    } else if net < 0.0 {
        PositionSide::Short
    } else {
        PositionSide::Flat
    };
    Ok(PositionStatusReport::new(
        account,
        instrument(),
        direction,
        qty(net.abs())?,
        now,
        now,
        None,
        None,
        None,
    ))
}

pub(crate) fn balances(value: &Value) -> anyhow::Result<(AccountBalance, MarginBalance, Params)> {
    let currency: Currency = text(value, "Currency")?.parse()?;
    let cash = number(value, "CashBalance")?;
    let total = number(value, "TotalValue")?;
    let used = number(value, "MarginUsedByCurrentPositions")?;
    ensure!(used >= 0.0, "invalid margin");
    // shortcut: LIVE balances 모양 미확인, PR 4 LIVE 읽기로 확인
    let initial = value
        .get("InitialMargin")
        .and_then(|v| v.get("MarginUsedByCurrentPositions"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(used);
    ensure!(initial >= 0.0, "invalid initial margin");
    let initial = Money::new_checked(initial, currency)?;
    let equity = Money::new_checked(total, currency)?;
    let used = Money::new_checked(used, currency)?;
    let free = equity.checked_sub(used).context("balance overflow")?;
    let balance = AccountBalance::new_checked(equity, used, free)?;
    let margin = MarginBalance::new_checked(initial, used, None)?;
    let info: Params =
        serde_json::from_value(serde_json::json!({"CashBalance": cash, "TotalValue": total}))?;
    Ok((balance, margin, info))
}

pub(crate) fn open_orders(
    value: &Value,
    account: AccountId,
    now: UnixNanos,
) -> anyhow::Result<Vec<OrderStatusReport>> {
    let mut output = Vec::new();
    for row in rows(value)? {
        if !supported(row) {
            continue;
        }
        let kind = match text(row, "OpenOrderType")? {
            "Stop" => OrderType::StopMarket,
            "Market" => OrderType::Market,
            "Limit" => OrderType::Limit,
            _ => anyhow::bail!("unsupported order type"),
        };
        let ts = timestamp(text(row, "OrderTime")?)?;
        let mut report = OrderStatusReport::new(
            account,
            instrument(),
            client(row),
            VenueOrderId::from(text(row, "OrderId")?),
            Some(side(row)?),
            kind,
            TimeInForce::Day,
            OrderStatus::Accepted,
            qty(number(row, "Amount")?)?,
            qty(0.0)?,
            ts,
            ts,
            now,
            None,
        );
        if kind == OrderType::StopMarket {
            report.trigger_price = Some(Price::new_checked(number(row, "Price")?, 3)?);
        } else if kind == OrderType::Limit {
            report.price = Some(Price::new_checked(number(row, "Price")?, 3)?);
        }
        output.push(report);
    }
    Ok(output)
}

pub(crate) fn fills(
    value: &Value,
    account: AccountId,
    now: UnixNanos,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    only_id: Option<VenueOrderId>,
) -> anyhow::Result<Vec<FillReport>> {
    use std::collections::HashMap;
    let mut sorted: Vec<_> = rows(value)?.iter().filter(|r| supported(r)).collect();
    sorted.sort_by_key(|r| r["ActivityTime"].as_str().unwrap_or("").to_owned());
    let mut totals: HashMap<String, f64> = HashMap::new();
    let mut output = Vec::new();
    for row in sorted {
        if !matches!(
            row["Status"].as_str(),
            Some("Fill" | "PartialFill" | "FinalFill")
        ) {
            continue;
        }
        let order_id = text(row, "OrderId")?;
        let cumulative = number(row, "FilledAmount")?;
        let old = totals.get(order_id).copied();
        let increment = match old {
            Some(old) => {
                ensure!(cumulative > old, "invalid cumulative fills");
                cumulative - old
            }
            None if start.is_some() => {
                let amount = number(row, "FillAmount")?;
                ensure!(amount > 0.0 && amount <= cumulative, "invalid fill amount");
                amount
            }
            None => cumulative,
        };
        totals.insert(order_id.to_owned(), cumulative);
        let time = timestamp(text(row, "ActivityTime")?)?;
        if start.is_some_and(|t| time < t)
            || end.is_some_and(|t| time > t)
            || only_id.is_some_and(|v| v.as_ref() != order_id)
        {
            continue;
        }
        output.push(FillReport::new(
            account,
            instrument(),
            VenueOrderId::from(order_id),
            TradeId::from(text(row, "LogId")?),
            side(row)?,
            qty(increment)?,
            Price::new_checked(number(row, "ExecutionPrice")?, 3)?,
            Money::new_checked(0.0, Currency::JPY())?,
            LiquiditySide::NoLiquiditySide,
            client(row),
            None,
            time,
            now,
            None,
        ));
    }
    Ok(output)
}

pub(crate) fn history_order(
    value: &Value,
    order: &nautilus_model::orders::OrderAny,
    id: &str,
    account: AccountId,
    now: UnixNanos,
) -> anyhow::Result<Option<OrderStatusReport>> {
    use nautilus_model::orders::Order;
    let mut activity: Vec<_> = rows(value)?
        .iter()
        .filter(|r| r["OrderId"].as_str() == Some(id))
        .collect();
    activity.sort_by_key(|r| r["ActivityTime"].as_str().unwrap_or("").to_owned());
    let Some(last) = activity.last() else {
        return Ok(None);
    };
    let last_ts = timestamp(text(last, "ActivityTime")?)?;
    let accepted = activity
        .iter()
        .find(|r| r["Status"].as_str() == Some("Placed"))
        .map_or(Ok(last_ts), |r| timestamp(text(r, "ActivityTime")?))?;
    let status = match text(last, "Status")? {
        "Placed" | "Changed" => OrderStatus::Accepted,
        "Fill" | "PartialFill" => OrderStatus::PartiallyFilled,
        "FinalFill" => OrderStatus::Filled,
        "Cancelled" => OrderStatus::Canceled,
        "Expired" | "DoneForDay" => OrderStatus::Expired,
        "Rejected" => OrderStatus::Rejected,
        _ => anyhow::bail!("unknown history status"),
    };
    let filled = activity
        .iter()
        .rev()
        .find_map(|row| row.get("FilledAmount").and_then(Value::as_f64))
        .unwrap_or(0.0);
    let mut report = OrderStatusReport::new(
        account,
        order.instrument_id(),
        client(last),
        VenueOrderId::from(id),
        Some(order.order_side()),
        order.order_type(),
        order.time_in_force(),
        status,
        order.quantity(),
        qty(filled)?,
        accepted,
        last_ts,
        now,
        None,
    );
    if order.order_type() == OrderType::StopMarket {
        report.trigger_price = activity
            .iter()
            .rev()
            .find_map(|r| r["Price"].as_f64())
            .map(|p| Price::new_checked(p, 3))
            .transpose()?;
    }
    Ok(Some(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(raw: &str) -> Value {
        serde_json::from_str::<Value>(raw).expect("fixture")["response"].clone()
    }
    #[test]
    fn open_stop_yields_one_order_status() {
        let value = response(include_str!(
            "../tests/fixtures/get_port_v1_orders_me_1.json"
        ));
        let reports = open_orders(&value, AccountId::from("SAXO-001"), 0.into()).expect("reports");
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].client_order_id,
            Some(ClientOrderId::from("r79-S1-S"))
        );
        assert_eq!(reports[0].order_type, OrderType::StopMarket);
        assert_eq!(reports[0].trigger_price, Some(Price::from("146.300")));
    }
    #[test]
    fn history_fill_yields_trade_report() {
        let value = response(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let reports = fills(
            &value,
            AccountId::from("SAXO-001"),
            0.into(),
            None,
            None,
            None,
        )
        .expect("reports");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].trade_id.to_string(), "1005");
        assert_eq!(reports[0].last_qty, Quantity::from(1000));
        assert_eq!(reports[0].last_px, Price::from("146.821"));
    }
    #[test]
    fn fill_reports_use_increment_before_start_filter() {
        let mut value = response(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        ));
        let rows = value["Data"].as_array_mut().expect("data");
        let mut first = rows[2].clone();
        first["Status"] = "Fill".into();
        first["FilledAmount"] = 400.into();
        first["FillAmount"] = 400.into();
        first["LogId"] = "1004".into();
        first["ActivityTime"] = "2030-01-02T14:19:35.849Z".into();
        rows.insert(2, first);
        let start = timestamp("2030-01-02T14:19:35.850Z").expect("time");
        let result = fills(
            &value,
            AccountId::from("SAXO-001"),
            0.into(),
            Some(start),
            None,
            None,
        )
        .expect("fills");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].last_qty, Quantity::from(600));
    }
    #[test]
    fn force_open_pair_returns_error_and_flat_is_reported() {
        let mut value = response(include_str!(
            "../tests/fixtures/get_port_v1_positions_me_2.json"
        ));
        value["Data"][0]["PositionBase"]["IsForceOpen"] = true.into();
        assert!(positions(&value, AccountId::from("SAXO-001"), 0.into()).is_err());
        value["Data"][0]["PositionBase"]["IsForceOpen"] = false.into();
        value["Data"][0]["PositionBase"]["Amount"] = 1000.into();
        let report = positions(&value, AccountId::from("SAXO-001"), 0.into()).expect("flat");
        assert_eq!(report.position_side, PositionSide::Flat);
        assert_eq!(report.quantity, Quantity::from(0));
    }
    #[test]
    fn balance_uses_distinct_margin_values_and_fallback() {
        let mut value = response(include_str!(
            "../tests/fixtures/get_port_v1_balances_me.json"
        ));
        value["InitialMargin"]["MarginUsedByCurrentPositions"] = 10.into();
        value["MarginUsedByCurrentPositions"] = 20.into();
        let (_, margin, _) = balances(&value).expect("margin");
        assert_eq!(
            margin.initial,
            Money::new_checked(10.0, Currency::EUR()).expect("money")
        );
        assert_eq!(
            margin.maintenance,
            Money::new_checked(20.0, Currency::EUR()).expect("money")
        );
        value["InitialMargin"]["MarginUsedByCurrentPositions"] = "unknown".into();
        let (_, margin, _) = balances(&value).expect("fallback");
        assert_eq!(margin.initial, margin.maintenance);
        value
            .as_object_mut()
            .expect("object")
            .remove("InitialMargin");
        let (_, margin, _) = balances(&value).expect("fallback");
        assert_eq!(margin.initial, margin.maintenance);
    }
    #[test]
    fn balances_require_all_fields() {
        let mut value = response(include_str!(
            "../tests/fixtures/get_port_v1_balances_me.json"
        ));
        let (balance, margin, info) = balances(&value).expect("balance");
        assert_eq!(
            balance.total,
            Money::new_checked(123.45, Currency::EUR()).expect("money")
        );
        assert_eq!(
            margin.initial,
            Money::new_checked(0.0, Currency::EUR()).expect("margin")
        );
        assert_eq!(info.get("TotalValue"), Some(&serde_json::json!(123.45)));
        for field in [
            "Currency",
            "CashBalance",
            "TotalValue",
            "MarginUsedByCurrentPositions",
        ] {
            let original = value[field].clone();
            value.as_object_mut().expect("object").remove(field);
            assert!(balances(&value).is_err());
            value[field] = original;
        }
        value["CashBalance"] = "bad".into();
        assert!(balances(&value).is_err());
    }
}

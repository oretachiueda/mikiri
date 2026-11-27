use forward::journal::{Action, CostStatus, Journal, Milli, Row, Side, Status};
use nautilus_model::{enums::OrderSide, events::OrderFilled};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug)]
pub struct FillContext {
    pub intent: Row,
    pub action: Action,
    pub entry_price_milli: Option<i64>,
    pub entry_side: Option<Side>,
    pub total_cost_milli: Option<i64>,
    pub equity_before_milli: Option<i64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Filled,
    Canceled,
    Expired,
}
#[derive(Debug)]
pub struct FillAccumulator {
    requested: i64,
    events: Vec<OrderFilled>,
    units: i64,
    weighted: i128,
}
impl FillAccumulator {
    pub fn new(requested: i64) -> anyhow::Result<Self> {
        anyhow::ensure!(requested > 0, "Human: invalid requested quantity");
        Ok(Self {
            requested,
            events: Vec::new(),
            units: 0,
            weighted: 0,
        })
    }
    pub fn push(&mut self, fill: OrderFilled) -> anyhow::Result<()> {
        if let Some(first) = self.events.first() {
            anyhow::ensure!(
                first.client_order_id == fill.client_order_id
                    && first.venue_order_id == fill.venue_order_id
                    && first.order_side == fill.order_side
                    && first.instrument_id == fill.instrument_id,
                "Human: inconsistent fill identity"
            );
            anyhow::ensure!(
                self.events.iter().all(|e| e.trade_id != fill.trade_id),
                "Human: duplicate trade"
            );
            anyhow::ensure!(
                self.events
                    .last()
                    .is_none_or(|e| e.ts_event <= fill.ts_event),
                "Human: out-of-order fill"
            );
        }
        let units = fill
            .last_qty
            .as_decimal()
            .normalize()
            .to_string()
            .parse::<i64>()?;
        let price: Milli = serde_json::from_str(&fill.last_px.to_string())?;
        anyhow::ensure!(units > 0 && price.0 > 0, "Human: invalid fill values");
        let total = self
            .units
            .checked_add(units)
            .ok_or_else(|| anyhow::anyhow!("Human: quantity overflow"))?;
        anyhow::ensure!(total <= self.requested, "Human: overfill");
        let value = i128::from(units)
            .checked_mul(i128::from(price.0))
            .and_then(|v| self.weighted.checked_add(v))
            .ok_or_else(|| anyhow::anyhow!("Human: price overflow"))?;
        self.units = total;
        self.weighted = value;
        self.events.push(fill);
        Ok(())
    }
    pub fn filled_units(&self) -> i64 {
        self.units
    }
    pub fn finish(
        self,
        journal: &Journal<'_>,
        context: FillContext,
        terminal: Terminal,
    ) -> anyhow::Result<()> {
        let mut row = context.intent;
        anyhow::ensure!(
            matches!(
                context.action,
                Action::Entry | Action::Exit | Action::StopExit
            ),
            "invalid fill action"
        );
        let last = self
            .events
            .last()
            .ok_or_else(|| anyhow::anyhow!("Human: missing fills"))?;
        anyhow::ensure!(
            row.idem_key == last.client_order_id.to_string(),
            "Human: fill key mismatch"
        );
        anyhow::ensure!(
            terminal != Terminal::Filled || self.units == self.requested,
            "Human: incomplete filled order"
        );
        let side = match last.order_side {
            OrderSide::Buy => Side::Buy,
            OrderSide::Sell => Side::Sell,
        };
        let timestamp =
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(last.ts_event.as_u64()))?
                .format(&Rfc3339)?;
        let average =
            i64::try_from((self.weighted + i128::from(self.units / 2)) / i128::from(self.units))?;
        // shortcut: hashes the Nautilus fill event, upgrade to the raw Saxo activity bytes when the adapter exposes them
        let evidence: Vec<_> = self
            .events
            .iter()
            .map(|fill| {
                (
                    fill.event_id.to_string(),
                    fill.trade_id.to_string(),
                    fill.venue_order_id.to_string(),
                    fill.client_order_id.to_string(),
                    fill.order_side.to_string(),
                    fill.last_qty.to_string(),
                    fill.last_px.to_string(),
                    fill.ts_event.as_u64(),
                )
            })
            .collect();
        let bytes = serde_json::to_vec(&evidence)?;
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        row.action = context.action;
        row.status = if self.units == self.requested {
            Status::Filled
        } else {
            Status::Partial
        };
        anyhow::ensure!(row.side == Some(side), "Human: fill side mismatch");
        match terminal {
            Terminal::Filled => {}
            Terminal::Canceled => row.reason = Some("canceled".into()),
            Terminal::Expired => row.reason = Some("expired".into()),
        }
        row.side = Some(side);
        row.units = Some(self.units);
        row.order_id = Some(last.venue_order_id.to_string());
        row.fill_time = Some(timestamp.clone());
        row.recorded_at = timestamp;
        row.fill_price = Some(Milli(average));
        row.source_sha256 = Some(digest);
        row.cost_status = Some(CostStatus::Pending);
        row.equity_before_jpy = context.equity_before_milli.map(Milli);
        if context.action != Action::Entry {
            let entry = context
                .entry_price_milli
                .ok_or_else(|| anyhow::anyhow!("Human: entry price unavailable"))?;
            let entry_side = context
                .entry_side
                .ok_or_else(|| anyhow::anyhow!("Human: entry direction unavailable"))?;
            let costs = context
                .total_cost_milli
                .ok_or_else(|| anyhow::anyhow!("Human: closing costs unavailable"))?;
            anyhow::ensure!(
                entry > 0 && costs >= 0 && side != entry_side,
                "Human: invalid closing fill"
            );
            let entry_value = i128::from(entry)
                .checked_mul(i128::from(self.units))
                .ok_or_else(|| anyhow::anyhow!("Human: pnl overflow"))?;
            let gross = if entry_side == Side::Buy {
                self.weighted.checked_sub(entry_value)
            } else {
                entry_value.checked_sub(self.weighted)
            }
            .ok_or_else(|| anyhow::anyhow!("Human: pnl overflow"))?;
            row.realized_pnl_jpy_ex_swap = Some(Milli(i64::try_from(
                gross
                    .checked_sub(i128::from(costs))
                    .ok_or_else(|| anyhow::anyhow!("Human: pnl overflow"))?,
            )?));
        }
        journal
            .append(&row)
            .map_err(|e| anyhow::anyhow!("journal: {e}"))
    }
}

use crate::forward_fills::{FillAccumulator, FillContext, Terminal};
use forward::journal::{Action, Journal, Row, Status};
use nautilus_model::events::OrderFilled;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Working,
    Done,
    Refused,
}
pub enum Event {
    Accepted,
    Updated,
    Filled {
        fill: Box<OrderFilled>,
        complete: bool,
    },
    Canceled,
    Expired,
    Rejected,
}
#[derive(Debug)]
pub struct OrderLifecycle {
    state: State,
    accumulator: Option<FillAccumulator>,
    context: Option<FillContext>,
}
impl OrderLifecycle {
    pub fn new(requested: i64, context: FillContext) -> anyhow::Result<Self> {
        Ok(Self {
            state: State::Pending,
            accumulator: Some(FillAccumulator::new(requested)?),
            context: Some(context),
        })
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn filled_units(&self) -> i64 {
        self.accumulator
            .as_ref()
            .map_or(0, FillAccumulator::filled_units)
    }
    pub fn on_event(&mut self, journal: &Journal<'_>, event: Event) -> anyhow::Result<()> {
        anyhow::ensure!(
            !matches!(self.state, State::Done | State::Refused),
            "Human: event after terminal state"
        );
        match event {
            Event::Accepted | Event::Updated => self.state = State::Working,
            Event::Filled { fill, complete } => {
                self.accumulator
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Human: missing accumulator"))?
                    .push(*fill)?;
                if complete {
                    self.finish(journal, Terminal::Filled)?;
                } else {
                    self.state = State::Working;
                }
            }
            Event::Canceled => {
                self.finish_or_record_empty(journal, Terminal::Canceled, "canceled")?
            }
            Event::Expired => self.finish_or_record_empty(journal, Terminal::Expired, "expired")?,
            Event::Rejected => {
                if self.filled_units() > 0 {
                    self.finish(journal, Terminal::Canceled)?;
                } else {
                    self.record_empty(journal, "rejected")?;
                    self.state = State::Refused;
                }
            }
        }
        Ok(())
    }
    fn finish_or_record_empty(
        &mut self,
        journal: &Journal<'_>,
        terminal: Terminal,
        reason: &str,
    ) -> anyhow::Result<()> {
        if self.filled_units() > 0 {
            self.finish(journal, terminal)
        } else {
            self.record_empty(journal, reason)
        }
    }
    fn record_empty(&mut self, journal: &Journal<'_>, reason: &str) -> anyhow::Result<()> {
        let context = self
            .context
            .take()
            .ok_or_else(|| anyhow::anyhow!("Human: missing context"))?;
        write_unfilled(journal, context.intent, context.action, reason)?;
        self.accumulator.take();
        self.state = State::Done;
        Ok(())
    }
    fn finish(&mut self, journal: &Journal<'_>, terminal: Terminal) -> anyhow::Result<()> {
        let context = self
            .context
            .take()
            .ok_or_else(|| anyhow::anyhow!("Human: missing context"))?;
        let accumulator = self
            .accumulator
            .take()
            .ok_or_else(|| anyhow::anyhow!("Human: missing accumulator"))?;
        accumulator.finish(journal, context, terminal)?;
        self.state = State::Done;
        Ok(())
    }
}

pub fn write_unfilled(
    journal: &Journal<'_>,
    mut row: Row,
    action: Action,
    reason: &str,
) -> anyhow::Result<()> {
    row.action = action;
    row.status = Status::Skipped;
    row.reason = Some(reason.into());
    row.units = None;
    row.side = None;
    row.recorded_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    journal.append(&row).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward_orders::tests::{intent, plan, synthetic_fill};
    use forward::journal::{Action, Environment, Journal, Milli};
    use std::collections::HashSet;
    #[test]
    fn partial_fills_are_journaled_only_after_terminal_cancellation() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let row = intent();
        let key = row.idem_key.clone();
        let _permit = j.begin(row.clone()).expect("begin");
        let ctx = FillContext {
            intent: row,
            action: Action::Entry,
            entry_price_milli: None,
            entry_side: None,
            total_cost_milli: None,
            equity_before_milli: Some(900_000),
        };
        let mut lifecycle = OrderLifecycle::new(20, ctx).expect("lifecycle");
        lifecycle.on_event(&j, Event::Accepted).expect("accepted");
        lifecycle
            .on_event(
                &j,
                Event::Filled {
                    fill: Box::new(synthetic_fill(&key, "T-1", 5, "10.000")),
                    complete: false,
                },
            )
            .expect("partial");
        lifecycle
            .on_event(
                &j,
                Event::Filled {
                    fill: Box::new(synthetic_fill(&key, "T-2", 5, "10.001")),
                    complete: false,
                },
            )
            .expect("partial");
        assert_eq!(lifecycle.filled_units(), 10);
        assert_eq!(j.read().expect("read").len(), 1);
        lifecycle.on_event(&j, Event::Canceled).expect("cancel");
        let rows = j.read().expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].status, Status::Partial);
        assert_eq!(rows[1].units, Some(10));
        assert_eq!(rows[1].fill_price, Some(Milli(10_001)));
        assert_eq!(lifecycle.state(), State::Done);
        assert!(lifecycle.on_event(&j, Event::Updated).is_err());
    }
    #[test]
    fn rejection_records_once_and_stops() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let row = intent();
        let _permit = j.begin(row.clone()).expect("begin");
        let ctx = FillContext {
            intent: row,
            action: Action::Entry,
            entry_price_milli: None,
            entry_side: None,
            total_cost_milli: None,
            equity_before_milli: None,
        };
        let mut lifecycle = OrderLifecycle::new(20, ctx).expect("lifecycle");
        lifecycle.on_event(&j, Event::Rejected).expect("reject");
        assert_eq!(lifecycle.state(), State::Refused);
        let rows = j.read().expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].reason.as_deref(), Some("rejected"));
        assert!(lifecycle.on_event(&j, Event::Rejected).is_err());
    }
}

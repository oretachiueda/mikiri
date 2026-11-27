use forward::{
    decide::{self, Input},
    journal::{Action, Environment, Journal, Row, Status},
    plan::Plan,
};
use std::path::Path;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
#[derive(Debug, PartialEq, Eq)]
pub enum Control {
    Continue,
    Halted,
    Killed,
}
fn record(
    journal: &Journal<'_>,
    plan: &Plan,
    now: OffsetDateTime,
    action: Action,
    status: Status,
) -> anyhow::Result<()> {
    let local = now.to_offset(plan.utc_offset);
    let now = now.to_offset(time::UtcOffset::UTC);
    let date = local.date();
    for leg in &plan.legs {
        let event = format!(
            "{}-{:04}{:02}{:02}",
            leg.name,
            date.year(),
            u8::from(date.month()),
            date.day()
        );
        let key = format!("{event}-ENTRY-SIM");
        let existing = journal
            .read_or_empty()
            .map_err(|e| anyhow::anyhow!("journal: {e}"))?;
        if existing
            .iter()
            .any(|r| r.idem_key == key && r.action == action)
        {
            continue;
        }
        let row = Row {
            version: 1,
            env: Environment::Sim,
            event_id: event,
            leg: leg.name.clone(),
            tokyo_date: date.to_string(),
            action,
            idem_key: key,
            status,
            recorded_at: now.format(&Rfc3339)?,
            declared_at: None,
            reason: None,
            side: None,
            units: None,
            order_id: None,
            fill_time: None,
            fill_price: None,
            commission_jpy: None,
            min_charge_jpy: None,
            cost_status: None,
            swap_jpy: None,
            swap_status: None,
            realized_pnl_jpy_ex_swap: None,
            equity_before_jpy: None,
            correction_of: None,
            source_sha256: None,
        };
        journal
            .append(&row)
            .map_err(|e| anyhow::anyhow!("journal: {e}"))?;
    }
    Ok(())
}
pub fn evaluate(
    plan: &Plan,
    journal: &Journal<'_>,
    input: &Input<'_>,
    kill_file: &Path,
    halt_file: Option<&Path>,
) -> anyhow::Result<Control> {
    if kill_file.try_exists()? || input.kill {
        record(journal, plan, input.now, Action::Kill, Status::Killed)?;
        return Ok(Control::Killed);
    }
    let halt_file_set = match halt_file {
        Some(p) => p.try_exists()?,
        None => false,
    };
    let equity = decide::equity_milli(plan, input)
        .ok_or_else(|| anyhow::anyhow!("Human: invalid equity"))?;
    if halt_file_set || input.halted || equity <= i128::from(plan.halt_threshold_milli) {
        record(journal, plan, input.now, Action::Halt, Status::Halted)?;
        return Ok(Control::Halted);
    }
    Ok(Control::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward_orders::tests::plan;
    use forward::replay::Replay;
    use std::{
        collections::{HashMap, HashSet},
        fs,
    };
    use time::macros::datetime;
    #[test]
    fn kill_is_durable_and_sticky_without_duplicate_records() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let state = Replay::empty(Environment::Sim);
        let input = state.input(datetime!(2098-01-01 02:00 UTC), "A", None, None, &p);
        let kill = dir.path().join("kill");
        fs::write(&kill, "").expect("kill");
        assert_eq!(
            evaluate(&p, &j, &input, &kill, None).expect("kill"),
            Control::Killed
        );
        assert_eq!(
            evaluate(&p, &j, &input, &kill, None).expect("kill again"),
            Control::Killed
        );
        assert_eq!(j.read().expect("read").len(), 1);
        assert!(j.replay(&HashMap::new()).expect("replay").killed);
    }
    #[test]
    fn loss_threshold_and_halt_file_create_halt() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let state = Replay::empty(Environment::Sim);
        let mut input = state.input(datetime!(2098-01-01 02:00 UTC), "A", None, None, &p);
        input.realized_milli = p.halt_threshold_milli - p.base_capital_milli;
        assert_eq!(
            evaluate(&p, &j, &input, &dir.path().join("kill"), None).expect("halt"),
            Control::Halted
        );
        assert!(j.replay(&HashMap::new()).expect("replay").halted);
        let other = Journal::new(
            dir.path().join("other"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        input.realized_milli = 0;
        let halt = dir.path().join("halt");
        fs::write(&halt, "").expect("halt");
        assert_eq!(
            evaluate(&p, &other, &input, &dir.path().join("kill"), Some(&halt)).expect("file halt"),
            Control::Halted
        );
    }
}

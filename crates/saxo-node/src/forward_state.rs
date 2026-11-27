use crate::forward_orders::reference;
use forward::{
    decide::Position,
    journal::{Action, Environment, Journal, Permit, Row, Side, Status},
    plan::{Direction, Plan},
    replay::Replay,
};
use std::{collections::HashMap, path::Path};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Debug)]
pub struct ForwardState {
    pub permits: HashMap<String, Permit>,
}
pub fn broker_matches(positions: &[Position], broker_units: i64) -> anyhow::Result<()> {
    let sum = positions.iter().try_fold(0_i64, |total, p| {
        total
            .checked_add(p.units)
            .ok_or_else(|| anyhow::anyhow!("Human: position overflow"))
    })?;
    anyhow::ensure!(
        sum == broker_units,
        "Human: journal and broker positions differ"
    );
    Ok(())
}
pub fn entry_intent(
    plan: &Plan,
    leg: &forward::plan::Leg,
    now: OffsetDateTime,
) -> anyhow::Result<Row> {
    let local = now.to_offset(plan.utc_offset);
    let now = now.to_offset(time::UtcOffset::UTC);
    let d = local.date();
    let event = format!(
        "{}-{:04}{:02}{:02}",
        leg.name,
        d.year(),
        u8::from(d.month()),
        d.day()
    );
    let key = format!("{event}-ENTRY-SIM");
    reference(&key)?;
    Ok(Row {
        version: 1,
        env: Environment::Sim,
        event_id: event,
        leg: leg.name.clone(),
        tokyo_date: d.to_string(),
        action: Action::Entry,
        idem_key: key,
        status: Status::Scheduled,
        recorded_at: now.format(&Rfc3339)?,
        declared_at: Some(now.format(&Rfc3339)?),
        reason: None,
        side: Some(match leg.direction {
            Direction::Buy => Side::Buy,
            Direction::Sell => Side::Sell,
        }),
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
    })
}
impl ForwardState {
    pub fn prepare(
        plan: &Plan,
        journal: &Journal<'_>,
        restored: &Replay,
        broker_units: i64,
        now: OffsetDateTime,
        kill_file: &Path,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(!kill_file.try_exists()?, "Human: kill file present");
        broker_matches(&restored.positions, broker_units)?;
        let local = now.to_offset(plan.utc_offset);
        let mut rows = Vec::new();
        for leg in &plan.legs {
            if plan.event_dates.contains(&local.date()) && local.time() < leg.entry.start {
                let row = entry_intent(plan, leg, now)?;
                if !restored.used_keys.contains(&row.idem_key) {
                    rows.push(row);
                }
            }
        }
        let mut permits = HashMap::new();
        for row in rows {
            anyhow::ensure!(!kill_file.try_exists()?, "Human: kill file present");
            let key = row.idem_key.clone();
            let permit = journal.begin(row).map_err(anyhow::Error::msg)?;
            permits.insert(key, permit);
        }
        anyhow::ensure!(!kill_file.try_exists()?, "Human: kill file present");
        Ok(Self { permits })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward_orders::tests::plan;
    use std::{
        collections::{HashMap, HashSet},
        fs,
    };
    use time::macros::datetime;
    #[test]
    fn prewindow_reserves_once_and_restart_has_no_permit() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let path = dir.path().join("journal");
        let j = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        let state = ForwardState::prepare(
            &p,
            &j,
            &Replay::empty(Environment::Sim),
            0,
            datetime!(2098-01-01 01:00 UTC),
            &dir.path().join("kill"),
        )
        .expect("prepare");
        assert!(state.permits.len() == 1);
        let rows = j.read().expect("read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, Action::Declare);
        assert_eq!(rows[0].units, None);
        let restored = j.replay(&HashMap::new()).expect("replay");
        let restart = ForwardState::prepare(
            &p,
            &j,
            &restored,
            0,
            datetime!(2098-01-01 01:00 UTC),
            &dir.path().join("kill"),
        )
        .expect("restart");
        assert!(restart.permits.is_empty());
        assert_eq!(j.read().expect("read").len(), 1);
    }
    #[test]
    fn late_kill_or_mismatch_never_create_declarations() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let path = dir.path().join("journal");
        let j = Journal::new(&path, Environment::Sim, &p, HashSet::new());
        let empty = Replay::empty(Environment::Sim);
        let kill = dir.path().join("kill");
        let late = ForwardState::prepare(&p, &j, &empty, 0, datetime!(2098-01-01 02:15 UTC), &kill)
            .expect("late");
        assert!(late.permits.is_empty());
        assert!(!path.exists());
        assert!(
            ForwardState::prepare(&p, &j, &empty, 1, datetime!(2098-01-01 01:00 UTC), &kill)
                .is_err()
        );
        assert!(!path.exists());
        fs::write(&kill, "").expect("kill");
        assert!(
            ForwardState::prepare(&p, &j, &empty, 0, datetime!(2098-01-01 01:00 UTC), &kill)
                .is_err()
        );
        assert!(!path.exists());
    }
}

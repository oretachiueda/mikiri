use forward::journal::{Action, Journal, Row, Status};

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Track,
    Filled,
    Missed,
}
pub fn reconcile_intent(
    journal: &Journal<'_>,
    declaration: &Row,
    broker_has_order: bool,
) -> anyhow::Result<Resolution> {
    anyhow::ensure!(
        declaration.action == Action::Declare
            && declaration.status == Status::Scheduled
            && declaration.side.is_some(),
        "Human: invalid declaration"
    );
    let rows = journal
        .read()
        .map_err(|e| anyhow::anyhow!("journal: {e}"))?;
    anyhow::ensure!(
        rows.iter().any(|r| r.idem_key == declaration.idem_key
            && r.action == Action::Declare
            && r.status == Status::Scheduled),
        "Human: declaration not recorded"
    );
    if rows.iter().any(|r| {
        r.idem_key == declaration.idem_key && matches!(r.status, Status::Filled | Status::Partial)
    }) {
        return Ok(Resolution::Filled);
    }
    if rows.iter().any(|r| {
        r.idem_key == declaration.idem_key && matches!(r.status, Status::Missed | Status::Skipped)
    }) {
        return Ok(Resolution::Missed);
    }
    if broker_has_order {
        return Ok(Resolution::Track);
    }
    let mut missed = declaration.clone();
    missed.recorded_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    missed.action = if declaration.idem_key.ends_with("-ENTRY-SIM") {
        Action::Entry
    } else {
        Action::Exit
    };
    missed.status = Status::Missed;
    missed.reason = Some("restart".into());
    missed.units = None;
    missed.side = None;
    missed.correction_of = None;
    journal
        .append(&missed)
        .map_err(|e| anyhow::anyhow!("journal: {e}"))?;
    Ok(Resolution::Missed)
}

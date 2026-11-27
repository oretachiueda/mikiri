use crate::forward_orders::{self, OrderContext};
use forward::{decide::Action, journal::Permit};
use nautilus_model::{
    identifiers::{ClientId, ClientOrderId},
    orders::OrderAny,
    types::Price,
};
use nautilus_trading::{Strategy, StrategyNative};
use std::path::Path;

pub trait Sink {
    fn submit(&mut self, orders: Vec<OrderAny>) -> anyhow::Result<()>;
    fn modify_stop(&mut self, key: ClientOrderId, trigger: Price) -> anyhow::Result<()>;
    fn cancel(&mut self, key: ClientOrderId) -> anyhow::Result<()>;
}
pub struct NautilusSink<'a, S>(pub &'a mut S);
impl<S: Strategy + StrategyNative> Sink for NautilusSink<'_, S> {
    fn submit(&mut self, mut orders: Vec<OrderAny>) -> anyhow::Result<()> {
        let client = Some(ClientId::new("SAXO"));
        match orders.len() {
            1 => self.0.submit_order(orders.remove(0), None, client, None),
            2 => self.0.submit_order_list(orders, None, client, None),
            _ => anyhow::bail!("Human: invalid order count"),
        }
    }
    fn modify_stop(&mut self, key: ClientOrderId, trigger: Price) -> anyhow::Result<()> {
        self.0.modify_order(
            key,
            None,
            None,
            Some(trigger),
            Some(ClientId::new("SAXO")),
            None,
        )
    }
    fn cancel(&mut self, key: ClientOrderId) -> anyhow::Result<()> {
        self.0.cancel_order(key, Some(ClientId::new("SAXO")), None)
    }
}
fn check_kill(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(!path.try_exists()?, "Human: kill file present");
    Ok(())
}
pub fn dispatch(
    sink: &mut impl Sink,
    action: Action,
    permit: Option<Permit>,
    stop_permit: Option<Permit>,
    context: OrderContext<'_>,
) -> anyhow::Result<()> {
    check_kill(context.kill_file)?;
    match action {
        action @ (Action::Open { .. } | Action::Submit { .. }) => {
            let permit = permit.ok_or_else(|| anyhow::anyhow!("Human: missing permit"))?;
            let kill = context.kill_file;
            let orders = forward_orders::build(permit, stop_permit, action, context)?;
            check_kill(kill)?;
            sink.submit(orders)
        }
        Action::MoveStop { key, trigger_milli } => {
            anyhow::ensure!(
                permit.is_none() && stop_permit.is_none() && trigger_milli > 0,
                "Human: invalid modify"
            );
            let id = forward_orders::reference(&key)?;
            let trigger = format!("{}.{:03}", trigger_milli / 1000, trigger_milli % 1000)
                .parse()
                .map_err(|e: String| anyhow::anyhow!("{e}"))?;
            check_kill(context.kill_file)?;
            sink.modify_stop(id, trigger)
        }
        Action::CancelStop { key } | Action::Cancel { key } => {
            anyhow::ensure!(
                permit.is_none() && stop_permit.is_none(),
                "Human: invalid cancel"
            );
            let id = forward_orders::reference(&key)?;
            check_kill(context.kill_file)?;
            sink.cancel(id)
        }
        _ => anyhow::bail!("Human: action is not a command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forward_orders::tests::{ids, intent, plan};
    use forward::{
        decide::Kind,
        journal::{Environment, Journal},
        plan::Direction,
    };
    use nautilus_model::{enums::OrderType, orders::Order};
    use std::{collections::HashSet, fs};
    #[derive(Default)]
    struct Fake {
        orders: Vec<OrderAny>,
        modified: Vec<(String, String)>,
        canceled: Vec<String>,
    }
    impl Sink for Fake {
        fn submit(&mut self, orders: Vec<OrderAny>) -> anyhow::Result<()> {
            self.orders.extend(orders);
            Ok(())
        }
        fn modify_stop(&mut self, key: ClientOrderId, trigger: Price) -> anyhow::Result<()> {
            self.modified.push((key.to_string(), trigger.to_string()));
            Ok(())
        }
        fn cancel(&mut self, key: ClientOrderId) -> anyhow::Result<()> {
            self.canceled.push(key.to_string());
            Ok(())
        }
    }
    fn context(path: &Path) -> OrderContext<'_> {
        let (trader, strategy, instrument, timestamp) = ids();
        OrderContext {
            trader,
            strategy,
            instrument,
            timestamp,
            kill_file: path,
        }
    }
    #[test]
    fn submit_modify_cancel_and_kill_are_guarded() {
        let dir = tempfile::tempdir().expect("dir");
        let p = plan(dir.path());
        let j = Journal::new(
            dir.path().join("journal"),
            Environment::Sim,
            &p,
            HashSet::new(),
        );
        let kill = dir.path().join("kill");
        let mut sink = Fake::default();
        let row = intent();
        let key = row.idem_key.clone();
        let permit = j.begin(row).expect("permit");
        dispatch(
            &mut sink,
            Action::Submit {
                key: key.clone(),
                kind: Kind::Entry,
                direction: Direction::Buy,
                units: 20,
                exit_late: false,
            },
            Some(permit),
            None,
            context(&kill),
        )
        .expect("submit");
        assert_eq!(sink.orders.len(), 1);
        assert_eq!(sink.orders[0].order_type(), OrderType::Market);
        assert_eq!(sink.orders[0].client_order_id().to_string(), key);
        dispatch(
            &mut sink,
            Action::MoveStop {
                key: key.clone(),
                trigger_milli: 12345,
            },
            None,
            None,
            context(&kill),
        )
        .expect("modify");
        assert_eq!(sink.modified[0].1, "12.345");
        dispatch(
            &mut sink,
            Action::Cancel { key: key.clone() },
            None,
            None,
            context(&kill),
        )
        .expect("cancel");
        assert_eq!(sink.canceled, vec![key.clone()]);
        fs::write(&kill, "").expect("kill");
        assert!(
            dispatch(
                &mut sink,
                Action::CancelStop { key },
                None,
                None,
                context(&kill)
            )
            .is_err()
        );
        assert_eq!(sink.canceled.len(), 1);
    }
    #[test]
    fn submit_without_permit_does_not_reach_sink() {
        let dir = tempfile::tempdir().expect("dir");
        let mut sink = Fake::default();
        let result = dispatch(
            &mut sink,
            Action::Submit {
                key: "A-ENTRY-SIM".into(),
                kind: Kind::Entry,
                direction: Direction::Buy,
                units: 20,
                exit_late: false,
            },
            None,
            None,
            context(&dir.path().join("kill")),
        );
        assert!(result.is_err());
        assert!(sink.orders.is_empty());
    }
}

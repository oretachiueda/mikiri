pub mod forward_control;
pub mod forward_dispatch;
pub mod forward_events;
pub mod forward_fills;
pub mod forward_orders;
pub mod forward_restart;
pub mod forward_state;
/// The state machine represents prices in millionths and trigger prices in thousandths.
pub mod roundtrip {
    pub const QUANTITY: u64 = 1_000;
    const MAX_COMMANDS: u8 = 4;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Phase {
        WaitingForQuote,
        WaitingForEntryAndStop,
        WaitingForUpdate,
        WaitingForCancel,
        WaitingForExit,
        WaitingForFlat { stop_triggered: bool },
        Finished { stop_triggered: bool },
        Rejected,
        Killed,
        TimedOut,
        CommandLimit,
        InvalidPrice,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct State {
        pub phase: Phase,
        pub bid_millionths: Option<i64>,
        pub entry_filled: bool,
        pub stop_accepted: bool,
        pub commands: u8,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                phase: Phase::WaitingForQuote,
                bid_millionths: None,
                entry_filled: false,
                stop_accepted: false,
                commands: 0,
            }
        }
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        FirmBid(i64),
        EntryFilled,
        StopAccepted,
        StopUpdated,
        StopCanceled,
        StopFilled,
        ExitFilled,
        PositionFlat(bool),
        EntryRejected,
        StopRejected,
        Kill,
        Timeout,
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Action {
        SubmitEntryAndStop {
            quantity: u64,
            trigger_thousandths: i64,
        },
        ModifyStop {
            trigger_thousandths: i64,
        },
        CancelStop,
        ClosePosition {
            quantity: u64,
        },
    }
    fn trigger(bid: i64, offset: i64) -> Option<i64> {
        bid.checked_sub(offset)
            .filter(|v| *v >= 1_000)
            .map(|v| v / 1_000)
    }
    fn command(state: &mut State, action: Action, actions: &mut Vec<Action>) {
        if state.commands >= MAX_COMMANDS {
            state.phase = Phase::CommandLimit;
        } else {
            state.commands += 1;
            actions.push(action);
        }
    }
    pub fn step(mut state: State, event: Event) -> (State, Vec<Action>) {
        let mut actions = Vec::new();
        if matches!(
            state.phase,
            Phase::Finished { .. }
                | Phase::Rejected
                | Phase::Killed
                | Phase::TimedOut
                | Phase::CommandLimit
                | Phase::InvalidPrice
        ) {
            return (state, actions);
        }
        match event {
            Event::Kill => {
                state.phase = Phase::Killed;
                return (state, actions);
            }
            Event::Timeout => {
                state.phase = Phase::TimedOut;
                return (state, actions);
            }
            Event::EntryRejected | Event::StopRejected => {
                state.phase = Phase::Rejected;
                return (state, actions);
            }
            Event::FirmBid(bid) => {
                if bid <= 0 {
                    state.phase = Phase::InvalidPrice;
                    return (state, actions);
                }
                state.bid_millionths = Some(bid);
                if state.phase == Phase::WaitingForQuote {
                    if let Some(price) = trigger(bid, 300_000) {
                        state.phase = Phase::WaitingForEntryAndStop;
                        command(
                            &mut state,
                            Action::SubmitEntryAndStop {
                                quantity: QUANTITY,
                                trigger_thousandths: price,
                            },
                            &mut actions,
                        );
                    } else {
                        state.phase = Phase::InvalidPrice;
                    }
                }
            }
            Event::EntryFilled if state.phase == Phase::WaitingForEntryAndStop => {
                state.entry_filled = true
            }
            Event::StopAccepted if state.phase == Phase::WaitingForEntryAndStop => {
                state.stop_accepted = true
            }
            Event::StopUpdated if state.phase == Phase::WaitingForUpdate => {
                state.phase = Phase::WaitingForCancel;
                command(&mut state, Action::CancelStop, &mut actions);
            }
            Event::StopCanceled if state.phase == Phase::WaitingForCancel => {
                state.phase = Phase::WaitingForExit;
                command(
                    &mut state,
                    Action::ClosePosition { quantity: QUANTITY },
                    &mut actions,
                );
            }
            Event::StopFilled
                if matches!(
                    state.phase,
                    Phase::WaitingForEntryAndStop
                        | Phase::WaitingForUpdate
                        | Phase::WaitingForCancel
                ) =>
            {
                state.phase = Phase::WaitingForFlat {
                    stop_triggered: true,
                }
            }
            Event::ExitFilled if state.phase == Phase::WaitingForExit => {
                state.phase = Phase::WaitingForFlat {
                    stop_triggered: false,
                }
            }
            Event::PositionFlat(true) => {
                if let Phase::WaitingForFlat { stop_triggered } = state.phase {
                    state.phase = Phase::Finished { stop_triggered };
                }
            }
            _ => {}
        }
        if state.phase == Phase::WaitingForEntryAndStop && state.entry_filled && state.stop_accepted
        {
            if let Some(price) = state.bid_millionths.and_then(|bid| trigger(bid, 250_000)) {
                state.phase = Phase::WaitingForUpdate;
                command(
                    &mut state,
                    Action::ModifyStop {
                        trigger_thousandths: price,
                    },
                    &mut actions,
                );
            } else {
                state.phase = Phase::InvalidPrice;
            }
        }
        (state, actions)
    }
}

use saxo_node::roundtrip::{Action, Event, Phase, State, step};

fn advance(state: &mut State, event: Event) -> Vec<Action> {
    let (next, actions) = step(*state, event);
    *state = next;
    actions
}
fn started() -> State {
    let mut s = State::default();
    assert_eq!(
        advance(&mut s, Event::FirmBid(155_456_700)),
        vec![Action::SubmitEntryAndStop {
            quantity: 1000,
            trigger_thousandths: 155_156
        }]
    );
    s
}
#[test]
fn normal_sequence_is_idempotent_and_waits_for_flat() {
    let mut s = started();
    assert!(advance(&mut s, Event::EntryFilled).is_empty());
    assert!(advance(&mut s, Event::EntryFilled).is_empty());
    assert!(advance(&mut s, Event::FirmBid(155_555_900)).is_empty());
    assert_eq!(
        advance(&mut s, Event::StopAccepted),
        vec![Action::ModifyStop {
            trigger_thousandths: 155_305
        }]
    );
    assert!(advance(&mut s, Event::StopAccepted).is_empty());
    assert_eq!(
        advance(&mut s, Event::StopUpdated),
        vec![Action::CancelStop]
    );
    assert!(advance(&mut s, Event::StopUpdated).is_empty());
    assert_eq!(
        advance(&mut s, Event::StopCanceled),
        vec![Action::ClosePosition { quantity: 1000 }]
    );
    assert!(advance(&mut s, Event::StopCanceled).is_empty());
    assert!(advance(&mut s, Event::ExitFilled).is_empty());
    assert!(advance(&mut s, Event::PositionFlat(false)).is_empty());
    assert_eq!(
        s.phase,
        Phase::WaitingForFlat {
            stop_triggered: false
        }
    );
    assert!(advance(&mut s, Event::PositionFlat(true)).is_empty());
    assert_eq!(
        s.phase,
        Phase::Finished {
            stop_triggered: false
        }
    );
    assert_eq!(s.commands, 4);
}
#[test]
fn reverse_acceptance_order_and_rejections() {
    let mut s = started();
    assert!(advance(&mut s, Event::StopAccepted).is_empty());
    assert_eq!(
        advance(&mut s, Event::EntryFilled),
        vec![Action::ModifyStop {
            trigger_thousandths: 155_206
        }]
    );
    for rejected in [Event::EntryRejected, Event::StopRejected] {
        let mut s = started();
        assert!(advance(&mut s, rejected).is_empty());
        assert_eq!(s.phase, Phase::Rejected);
        for e in [
            Event::EntryFilled,
            Event::StopAccepted,
            Event::FirmBid(155_456_700),
        ] {
            assert!(advance(&mut s, e).is_empty());
        }
    }
}
#[test]
fn stop_filled_before_cancel_never_closes_again() {
    for stage in 0..3 {
        let mut s = started();
        if stage >= 1 {
            advance(&mut s, Event::EntryFilled);
            advance(&mut s, Event::StopAccepted);
        }
        if stage >= 2 {
            advance(&mut s, Event::StopUpdated);
        }
        assert!(advance(&mut s, Event::StopFilled).is_empty());
        for e in [Event::StopCanceled, Event::StopFilled, Event::ExitFilled] {
            assert!(advance(&mut s, e).is_empty());
        }
        advance(&mut s, Event::PositionFlat(true));
        assert_eq!(
            s.phase,
            Phase::Finished {
                stop_triggered: true
            }
        );
    }
}
#[test]
fn kill_and_timeout_stop_all_active_phases() {
    for phase in [
        Phase::WaitingForQuote,
        Phase::WaitingForEntryAndStop,
        Phase::WaitingForUpdate,
        Phase::WaitingForCancel,
        Phase::WaitingForExit,
        Phase::WaitingForFlat {
            stop_triggered: true,
        },
        Phase::WaitingForFlat {
            stop_triggered: false,
        },
    ] {
        for (event, result) in [
            (Event::Kill, Phase::Killed),
            (Event::Timeout, Phase::TimedOut),
        ] {
            let s = State {
                phase,
                bid_millionths: Some(155_456_700),
                entry_filled: true,
                stop_accepted: true,
                commands: 3,
            };
            let (s, actions) = step(s, event);
            assert!(actions.is_empty());
            assert_eq!(s.phase, result);
            assert!(step(s, Event::StopUpdated).1.is_empty());
        }
    }
}
#[test]
fn fifth_command_is_refused_and_price_is_checked() {
    let s = State {
        phase: Phase::WaitingForCancel,
        commands: 4,
        ..State::default()
    };
    let (s, actions) = step(s, Event::StopCanceled);
    assert!(actions.is_empty());
    assert_eq!(s.phase, Phase::CommandLimit);
    assert_eq!(s.commands, 4);
    for bid in [0, -1, 300_999] {
        let (s, actions) = step(State::default(), Event::FirmBid(bid));
        assert!(actions.is_empty());
        assert_eq!(s.phase, Phase::InvalidPrice);
    }
}

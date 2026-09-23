//! Field failures as sub-second tests.

use warpaxe_scenarios::{fast_limits, max_settings, Bench};
use warpaxe_sim::{BoardFault, Fault};
use warppool_tune::Action;

/// No faults: from stock the regulator climbs, the device follows, no
/// reboot. Neighbouring steps are seeded as known-good so the climb
/// runs on the dwell instead of waiting for a judgement per step.
#[test]
fn a_healthy_gamma_climbs_from_stock() {
    let mut b = Bench::gamma(max_settings(70.0, 30.0, 690), fast_limits(), (525, 1150), 1);
    b.reg.seed_known_good(&[550, 575, 600, 625]);
    b.run_for(400);
    let ups = b.of(Action::Up);
    assert!(ups.len() >= 2, "expected a climb, got:\n{}", b.transcript());
    assert!(
        b.device_point().0 > 525,
        "device followed: {:?}",
        b.device_point()
    );
    assert_eq!(
        b.restarts,
        0,
        "no reboot on a healthy device:\n{}",
        b.transcript()
    );
    assert!(b.device.board().chip_running());
}

/// The Gaia of 2026-09-22 and the live run of 2026-09-23 (`--seed 5`),
/// in one test: the step past the threshold kills the VR silently, the
/// power-collapse guard catches it inside the settle window, the retreat
/// does not revive the board, the dead-chip guard reboots it — and the
/// collapse point becomes a ceiling, not a hole: no second attempt above
/// it, no second reboot.
#[test]
fn silent_vr_death_is_caught_in_the_settle_window_and_becomes_a_ceiling() {
    let mut b = Bench::gamma(max_settings(70.0, 30.0, 690), fast_limits(), (600, 1200), 5);
    b.board_fault(BoardFault::SilentVrShutdown {
        above_hz: 600_000_000,
    });
    b.run_for(600);
    let t = b.transcript();

    let up = b
        .of(Action::Up)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no step up at all:\n{t}"));
    assert_eq!(up.decision.to.0, 625, "first step goes to 625:\n{t}");

    let collapse = b
        .first_reason("power collapse")
        .unwrap_or_else(|| panic!("power-collapse guard never fired:\n{t}"));
    assert!(
        collapse.at_s - up.at_s <= 30,
        "collapse caught within a few decisions of the step ({} s after):\n{t}",
        collapse.at_s - up.at_s
    );
    assert!(collapse.decision.urgent);
    assert_eq!(
        collapse.decision.to.0, 600,
        "retreats to the last running point"
    );

    let dead = b
        .first_reason("chip not hashing")
        .unwrap_or_else(|| panic!("dead-chip guard never fired:\n{t}"));
    assert!(dead.decision.restart, "…and asks for a reboot");
    assert!(dead.at_s > collapse.at_s);

    // The ceiling: nothing at or above 625 is tried again.
    let later_ups: Vec<u16> = b
        .of(Action::Up)
        .iter()
        .filter(|a| a.at_s > dead.at_s)
        .map(|a| a.decision.to.0)
        .collect();
    assert!(
        later_ups.iter().all(|&f| f < 625),
        "climbed back above the collapse point: {later_ups:?}\n{t}"
    );
    assert_eq!(b.restarts, 1, "exactly one reboot, not two:\n{t}");
    let bad = b.reg.bad_steps(b.now_s());
    assert!(
        bad.contains(&625) && bad.contains(&650),
        "ceiling in the step memory: {bad:?}"
    );

    // Ground truth: alive again at 600.
    assert!(b.device.board().chip_running(), "device recovered:\n{t}");
    assert_eq!(b.device_point().0, 600);
}

/// A chip that hangs above a clock while the board keeps drawing power:
/// no collapse to see, only the hashrate going to zero. The dead-chip
/// guard's hashrate leg catches it, reboots, and the step is a ceiling.
#[test]
fn a_hung_chip_with_power_still_drawn_is_caught_by_the_hashrate_leg() {
    let mut b = Bench::gamma(max_settings(70.0, 30.0, 690), fast_limits(), (600, 1200), 7);
    b.chip_fault(Fault::HangAbove { hz: 600_000_000 });
    b.run_for(600);
    let t = b.transcript();

    let up = b
        .of(Action::Up)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no step up:\n{t}"));
    assert_eq!(up.decision.to.0, 625);
    assert!(
        b.first_reason("power collapse").is_none(),
        "no power collapse on a hang — the board still draws:\n{t}"
    );
    let dead = b
        .first_reason("chip not hashing")
        .unwrap_or_else(|| panic!("dead-chip guard never fired:\n{t}"));
    assert!(dead.decision.restart);
    assert!(b.restarts >= 1);
    let later_ups: Vec<u16> = b
        .of(Action::Up)
        .iter()
        .filter(|a| a.at_s > dead.at_s)
        .map(|a| a.decision.to.0)
        .collect();
    assert!(
        later_ups.iter().all(|&f| f < 625),
        "ceiling holds: {later_ups:?}\n{t}"
    );
    assert!(b.device.board().chip_running());
}

/// A chip sensor stuck at 0 (the Gaia's `asicTemps: [0]`): the sidecar
/// refuses to build a sample without a temperature, so the regulator
/// must never write to a device it cannot read. Blind is not brave.
#[test]
fn a_stuck_temperature_sensor_stops_the_regulator_cold() {
    let mut b = Bench::gamma(max_settings(70.0, 30.0, 690), fast_limits(), (525, 1150), 3);
    b.board_fault(BoardFault::ChipSensorStuck { at_c: 0.0 });
    b.run_for(300);
    assert!(
        b.sample_errors > 100,
        "every sample rejected: {}",
        b.sample_errors
    );
    assert!(
        b.writes.is_empty(),
        "no writes to a blind device:\n{}",
        b.transcript()
    );
    assert_eq!(b.restarts, 0);
    assert_eq!(b.device_point(), (525, 1150), "device untouched");
}

/// Duplicate nonces are rejects, and rejects are instability: the
/// climb is blocked and the regulator steps down rather than up.
#[test]
fn duplicate_nonces_block_the_climb() {
    let mut b = Bench::gamma(
        max_settings(70.0, 30.0, 690),
        fast_limits(),
        (600, 1200),
        11,
    );
    b.reg.seed_known_good(&[625, 650]);
    b.chip_fault(Fault::DuplicateNonce { per_mille: 500 });
    b.run_for(600);
    let t = b.transcript();
    assert!(
        b.of(Action::Up).is_empty(),
        "no climb on a 50 % reject rate:\n{t}"
    );
    let blocked = b
        .applied
        .iter()
        .chain(std::iter::empty())
        .any(|a| a.decision.reason.to_lowercase().contains("reject"))
        || !b.of(Action::Down).is_empty();
    assert!(
        blocked,
        "rejects should show up as a blocker or a step down:\n{t}"
    );
    assert_eq!(b.restarts, 0, "rejects are not a dead chip:\n{t}");
}

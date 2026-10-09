//! The chip rails off and on again on the Nerd carriers (TPS53647 /
//! TPS53667 with the chips' I/O rails on an LDO): the order and the
//! pauses, and the longer pause after a warm reboot.
//!
//! The reference is NerdOS. Its `NerdQaxePlus::shutdown` — the only way
//! it switches the chips off, and what it runs before every
//! `esp_restart` — drops the core rail, waits 500 ms, drops the LDO,
//! waits 500 ms. Its `initAsics` starts with everything off and the
//! chain in reset, waits 250 ms, LDO on, 100 ms, regulator init and
//! the core rail, 500 ms, reset released. NerdOS has nothing between
//! those two: no chips off and on without a reboot.
//!
//! WarpAxe switches the chips off and on in place (the web UI, the API,
//! an overheat stop). Until v0.1.23 that cut the core rail only: the LDO
//! stayed up, the reset line stayed high. On the NerdOctaxe-γ (.45,
//! 2026-10-09) a hash domain that came up dead after an OTA reboot then
//! survived five such re-inits with the core off for up to 15 s, and
//! cleared only with a power cycle of a few seconds. So the off state
//! here is NerdOS's shutdown — reset low, core off, LDO off — and the
//! way back is its `initAsics` order.
//!
//! A warm reboot (OTA, restart, download mode) keeps the 12 V input and
//! with it everything the board feeds from it: the regulator's own
//! supply, the LDO inputs, and whatever the ESP's lines back-feed while
//! they float through the reset. The firmware cannot cut the 12 V, but
//! it can give the rails the seconds off a power cycle gives them before
//! it brings the LDO back: counted from the moment the boot code drove
//! enable and LDO low, so the seconds the boot spends on USB and Wi-Fi
//! count towards it.

/// NerdOS `initAsics`: everything off and the chain in reset, then this
/// long before the LDO comes on.
pub const NERD_SETTLE_MS: u32 = 250;
/// NerdOS `initAsics`: the LDO on, then this long before the regulator
/// is set up and enabled.
pub const LDO_TO_CORE_ON_MS: u32 = 100;
/// NerdOS `shutdown`: the core rail off, then this long before the LDO.
pub const CORE_TO_LDO_OFF_MS: u32 = 500;
/// NerdOS `shutdown`: the LDO off, then this long before `esp_restart`.
pub const LDO_OFF_BEFORE_RESET_MS: u32 = 500;
/// How long enable and LDO stay low after a reset that was not a
/// power-on before the LDO comes back: a power cycle's "off for a few
/// seconds". A first value for the bench on the .45; counted from the
/// pins going low, not from the bring-up.
pub const WARM_OFF_MS: u32 = 5000;

/// How long to wait before the LDO comes on at the bring-up.
/// `power_on`: the ESP's reset reason is a power-on, so the rails were
/// really down. `low_for_ms`: how long enable and LDO have been driven
/// low already. Never less than NerdOS's 250 ms.
pub fn settle_ms(power_on: bool, low_for_ms: u32) -> u32 {
    if power_on {
        return NERD_SETTLE_MS;
    }
    WARM_OFF_MS.saturating_sub(low_for_ms).max(NERD_SETTLE_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cold_start_waits_nerdos_250_ms() {
        assert_eq!(settle_ms(true, 0), 250);
        assert_eq!(settle_ms(true, 10_000), 250);
    }

    #[test]
    fn a_warm_reboot_makes_up_the_seconds_a_power_cycle_gives() {
        assert_eq!(settle_ms(false, 0), 5000);
        assert_eq!(settle_ms(false, 1500), 3500);
        assert_eq!(settle_ms(false, 4000), 1000);
    }

    #[test]
    fn a_warm_reboot_never_waits_less_than_nerdos() {
        assert_eq!(settle_ms(false, 4900), 250);
        assert_eq!(settle_ms(false, 5000), 250);
        assert_eq!(settle_ms(false, u32::MAX), 250);
    }

    #[test]
    fn the_off_order_is_nerdos_shutdown() {
        // Core first, LDO half a second later, half a second more before
        // a reset: the sum is what NerdOS spends in shutdown().
        assert_eq!(CORE_TO_LDO_OFF_MS + LDO_OFF_BEFORE_RESET_MS, 1000);
        // The LDO's lead over the core on the way back is NerdOS's too.
        assert_eq!(LDO_TO_CORE_ON_MS, 100);
    }
}

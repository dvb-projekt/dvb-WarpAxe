//! An external regulator's heartbeat: WarpTune says "I am regulating
//! this miner" with a time to live, and the firmware reports whether
//! that still holds (`externalRegulator` in `/api/system/info`).
//!
//! Only an expiry on the uptime clock, in RAM: nothing is written to
//! NVS, the display is not woken, no event is recorded, and a reboot
//! starts with no heartbeat. Its only effect for now is the report; the
//! firmware's own base regulation (a later block) will stand back while
//! it is active.

/// Time to live when the request names none, s.
pub const DEFAULT_TTL_S: u32 = 600;
/// Shortest time to live a request can set, s; less is raised to it.
pub const MIN_TTL_S: u32 = 60;
/// Longest, s; more is lowered to it.
pub const MAX_TTL_S: u32 = 3600;

/// The heartbeat's state: when it expires, on the uptime clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Heartbeat {
    expires_at_s: u64,
}

/// The heartbeat as the API reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// A heartbeat was received and has not expired.
    pub active: bool,
    /// Seconds until it expires; 0 when inactive.
    pub expires_in_s: u32,
}

impl Heartbeat {
    /// No heartbeat, as after a boot.
    pub const fn new() -> Self {
        Self { expires_at_s: 0 }
    }

    /// The expiry on the uptime clock, s; 0 when none. For a store that
    /// keeps the value in an atomic.
    pub const fn expires_at_s(&self) -> u64 {
        self.expires_at_s
    }

    /// From a stored expiry (see [`Heartbeat::expires_at_s`]).
    pub const fn from_expires_at_s(expires_at_s: u64) -> Self {
        Self { expires_at_s }
    }

    /// A heartbeat at uptime `now_s` with the time to live the request
    /// gave, `None` for the default, clamped to the allowed range; 0
    /// ends the heartbeat at once.
    pub fn beat(&mut self, now_s: u64, ttl_s: Option<u32>) {
        self.expires_at_s = match ttl_s {
            Some(0) => 0,
            Some(t) => now_s + u64::from(t.clamp(MIN_TTL_S, MAX_TTL_S)),
            None => now_s + u64::from(DEFAULT_TTL_S),
        };
    }

    /// Active, and for how long, at uptime `now_s`.
    pub fn status(&self, now_s: u64) -> Status {
        let left = self.expires_at_s.saturating_sub(now_s);
        Status {
            active: left > 0,
            expires_in_s: left.min(u64::from(u32::MAX)) as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_heartbeat_after_a_boot() {
        let h = Heartbeat::new();
        assert_eq!(
            h.status(0),
            Status {
                active: false,
                expires_in_s: 0
            }
        );
        assert!(!h.status(1_000_000).active);
    }

    #[test]
    fn a_beat_without_a_ttl_lasts_ten_minutes() {
        let mut h = Heartbeat::new();
        h.beat(100, None);
        assert_eq!(
            h.status(100),
            Status {
                active: true,
                expires_in_s: 600
            }
        );
        assert_eq!(h.status(699).expires_in_s, 1);
        assert_eq!(
            h.status(700),
            Status {
                active: false,
                expires_in_s: 0
            }
        );
        assert!(!h.status(5000).active);
    }

    #[test]
    fn the_ttl_is_clamped_to_a_minute_and_an_hour() {
        let mut h = Heartbeat::new();
        h.beat(10, Some(5));
        assert_eq!(h.status(10).expires_in_s, 60);
        h.beat(10, Some(60));
        assert_eq!(h.status(10).expires_in_s, 60);
        h.beat(10, Some(3600));
        assert_eq!(h.status(10).expires_in_s, 3600);
        h.beat(10, Some(86_400));
        assert_eq!(h.status(10).expires_in_s, 3600);
        h.beat(10, Some(412));
        assert_eq!(h.status(10).expires_in_s, 412);
    }

    #[test]
    fn a_zero_ttl_ends_the_heartbeat_at_once() {
        let mut h = Heartbeat::new();
        h.beat(10, Some(600));
        assert!(h.status(11).active);
        h.beat(11, Some(0));
        assert_eq!(
            h.status(11),
            Status {
                active: false,
                expires_in_s: 0
            }
        );
    }

    #[test]
    fn a_renewal_moves_the_expiry_not_adds_to_it() {
        let mut h = Heartbeat::new();
        h.beat(0, Some(600));
        h.beat(120, Some(600));
        assert_eq!(h.status(120).expires_in_s, 600);
        assert!(!h.status(720).active);
    }

    #[test]
    fn the_expiry_survives_a_round_trip_through_a_store() {
        let mut h = Heartbeat::new();
        h.beat(50, Some(300));
        let back = Heartbeat::from_expires_at_s(h.expires_at_s());
        assert_eq!(back, h);
        assert_eq!(back.status(100).expires_in_s, 250);
    }
}

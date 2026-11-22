//! When a message goes out.
//!
//! - A new best difficulty comes in bursts (after a reset of the best,
//!   every share is one): at most one message per [`BEST_GAP_S`], saying
//!   the newest value ([`Coalesce`]).
//! - Pool switches can flap: at most one message per [`POOL_GAP_S`], the
//!   newest state, with the number of switches folded into it.
//! - A send that failed on the way (no connection, a timeout, a 5xx) is
//!   tried once more after [`RETRY_S`]; Discord's "too many requests"
//!   (429) once more after its `retry_after`, if that is at most
//!   [`MAX_WAIT_S`]. Anything else is dropped with a log line
//!   ([`after`]).
//! - Between two sends at least [`SPACING_S`] (Discord allows five per
//!   two seconds per webhook).

use serde::Serialize;

/// Seconds between two best-difficulty messages.
pub const BEST_GAP_S: u64 = 600;
/// Seconds between two pool-switch messages.
pub const POOL_GAP_S: u64 = 60;
/// Seconds before the one retry after a failed send.
pub const RETRY_S: u64 = 5;
/// The longest `retry_after` waited for; a longer one drops the message.
pub const MAX_WAIT_S: u64 = 60;
/// Seconds between two sends.
pub const SPACING_S: u64 = 1;
/// Seconds between two test messages.
pub const TEST_GAP_S: u64 = 5;

/// Messages of one kind folded into one per `gap` seconds: what is
/// offered while the gap runs replaces what waits, and the next due
/// message is the newest, with the count of those it replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Coalesce<T> {
    gap_s: u64,
    last_sent: Option<u64>,
    pending: Option<T>,
    skipped: u32,
}

impl<T> Coalesce<T> {
    /// Nothing waiting, nothing sent yet: the first offer is due at once.
    pub const fn new(gap_s: u64) -> Self {
        Coalesce {
            gap_s,
            last_sent: None,
            pending: None,
            skipped: 0,
        }
    }

    /// A new message; one still waiting is replaced (and counted).
    pub fn offer(&mut self, item: T) {
        if self.pending.replace(item).is_some() {
            self.skipped = self.skipped.saturating_add(1);
        }
    }

    /// The waiting message and how many it replaced, if the gap since
    /// the last one (`now` in seconds of uptime) has run out.
    pub fn take_due(&mut self, now: u64) -> Option<(T, u32)> {
        self.pending.as_ref()?;
        if self
            .last_sent
            .is_some_and(|t| now < t.saturating_add(self.gap_s))
        {
            return None;
        }
        self.last_sent = Some(now);
        let skipped = std::mem::take(&mut self.skipped);
        self.pending.take().map(|p| (p, skipped))
    }

    /// Something is waiting.
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget what waits (the switch went off, the webhook was cleared).
    pub fn clear(&mut self) {
        self.pending = None;
        self.skipped = 0;
    }
}

/// What to do after a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Delivered.
    Done,
    /// Try once more after this many seconds.
    Retry(u64),
    /// Dropped; no send before `pause_s` seconds have passed (Discord's
    /// `retry_after` on a 429, else 0).
    Drop {
        /// Seconds of quiet asked for.
        pause_s: u64,
    },
}

/// After attempt `attempt` (1 or 2) ended with HTTP `status` (`None`: no
/// answer, a connection or TLS failure, a timeout) and, on a 429,
/// Discord's `retry_after` in seconds.
pub fn after(status: Option<u16>, attempt: u32, retry_after_s: Option<f64>) -> Next {
    match status {
        Some(200..=299) => Next::Done,
        Some(429) => {
            let wait = retry_after_s
                .filter(|s| s.is_finite() && *s >= 0.0)
                .map_or(RETRY_S, |s| s.ceil().max(1.0) as u64);
            if attempt <= 1 && wait <= MAX_WAIT_S {
                Next::Retry(wait)
            } else {
                Next::Drop { pause_s: wait }
            }
        }
        None | Some(500..=599) if attempt <= 1 => Next::Retry(RETRY_S),
        _ => Next::Drop { pause_s: 0 },
    }
}

/// Discord's `retry_after` (seconds, a fraction allowed) from a 429's
/// JSON body, else from the `Retry-After` header.
pub fn retry_after(header: Option<&str>, body: &[u8]) -> Option<f64> {
    let from_body = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("retry_after").and_then(|r| r.as_f64()));
    from_body
        .or_else(|| header.and_then(|h| h.trim().parse::<f64>().ok()))
        .filter(|s| s.is_finite() && *s >= 0.0)
}

/// The last send's result, as `/api/system/info` shows it in
/// `discordLastResult`. Never carries the URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastResult {
    /// Delivered (2xx).
    pub ok: bool,
    /// The HTTP status, `None` without an answer.
    pub code: Option<u16>,
    /// What went wrong, in short; `None` when delivered.
    pub error: Option<String>,
    /// The message's kind ([`crate::Kind::name`]).
    pub what: &'static str,
    /// Unix seconds; 0 before NTP set the clock.
    pub t: u64,
    /// Uptime seconds.
    pub uptime: u64,
}

impl LastResult {
    /// The result of one send: `status` as for [`after`], `error` the
    /// transport's error text when there was no answer.
    pub fn new(
        what: &'static str,
        status: Option<u16>,
        error: Option<String>,
        t: u64,
        uptime: u64,
    ) -> Self {
        let ok = matches!(status, Some(200..=299));
        let error = if ok {
            None
        } else {
            Some(match (status, error) {
                (_, Some(e)) if !e.is_empty() => e,
                (Some(401), _) | (Some(404), _) => {
                    "Discord does not know this webhook (deleted?)".into()
                }
                (Some(429), _) => "rate limited by Discord".into(),
                (Some(c), _) => format!("HTTP {c}"),
                (None, _) => "no connection".into(),
            })
        };
        LastResult {
            ok,
            code: status,
            error,
            what,
            t,
            uptime,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_goes_at_once_then_one_per_gap_with_the_newest() {
        let mut c = Coalesce::new(BEST_GAP_S);
        assert_eq!(c.take_due(100), None);
        c.offer(1.0);
        assert_eq!(c.take_due(100), Some((1.0, 0)));
        // A burst inside the gap: held, the newest wins.
        c.offer(2.0);
        c.offer(3.0);
        c.offer(4.0);
        assert!(c.waiting());
        assert_eq!(c.take_due(101), None);
        assert_eq!(c.take_due(100 + BEST_GAP_S - 1), None);
        assert_eq!(c.take_due(100 + BEST_GAP_S), Some((4.0, 2)));
        assert!(!c.waiting());
        // Quiet for longer than the gap: the next goes at once again.
        c.offer(5.0);
        assert_eq!(c.take_due(100 + 3 * BEST_GAP_S), Some((5.0, 0)));
    }

    #[test]
    fn clearing_drops_what_waits() {
        let mut c = Coalesce::new(POOL_GAP_S);
        c.offer("a");
        assert!(c.take_due(0).is_some());
        c.offer("b");
        c.offer("c");
        c.clear();
        assert_eq!(c.take_due(1000), None);
        c.offer("d");
        assert_eq!(c.take_due(1000), Some(("d", 0)));
    }

    #[test]
    fn one_retry_at_most() {
        assert_eq!(after(Some(204), 1, None), Next::Done);
        assert_eq!(after(Some(200), 2, None), Next::Done);
        assert_eq!(after(None, 1, None), Next::Retry(RETRY_S));
        assert_eq!(after(None, 2, None), Next::Drop { pause_s: 0 });
        assert_eq!(after(Some(502), 1, None), Next::Retry(RETRY_S));
        assert_eq!(after(Some(502), 2, None), Next::Drop { pause_s: 0 });
        // The owner's mistake or a deleted webhook: no retry.
        assert_eq!(after(Some(400), 1, None), Next::Drop { pause_s: 0 });
        assert_eq!(after(Some(401), 1, None), Next::Drop { pause_s: 0 });
        assert_eq!(after(Some(404), 1, None), Next::Drop { pause_s: 0 });
    }

    #[test]
    fn too_many_requests_waits_as_discord_says() {
        assert_eq!(after(Some(429), 1, Some(0.3)), Next::Retry(1));
        assert_eq!(after(Some(429), 1, Some(2.5)), Next::Retry(3));
        assert_eq!(after(Some(429), 1, None), Next::Retry(RETRY_S));
        assert_eq!(after(Some(429), 1, Some(f64::NAN)), Next::Retry(RETRY_S));
        // Too long a wait, or already the retry: dropped, quiet that long.
        assert_eq!(
            after(Some(429), 1, Some(600.0)),
            Next::Drop { pause_s: 600 }
        );
        assert_eq!(after(Some(429), 2, Some(2.0)), Next::Drop { pause_s: 2 });
    }

    #[test]
    fn retry_after_is_read_from_the_body_or_the_header() {
        let body =
            br#"{"message": "You are being rate limited.", "retry_after": 1.234, "global": false}"#;
        assert_eq!(retry_after(None, body), Some(1.234));
        assert_eq!(retry_after(Some("7"), body), Some(1.234));
        assert_eq!(retry_after(Some(" 7 "), b""), Some(7.0));
        assert_eq!(retry_after(Some("soon"), b"not json"), None);
        assert_eq!(retry_after(None, br#"{"retry_after": -1}"#), None);
    }

    #[test]
    fn the_last_result_says_what_happened() {
        let ok = LastResult::new("test", Some(204), None, 1_700_000_000, 42);
        assert!(ok.ok);
        assert_eq!(ok.error, None);
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"ok": true, "code": 204, "error": null, "what": "test", "t": 1_700_000_000u64, "uptime": 42})
        );
        let gone = LastResult::new("block", Some(404), None, 0, 1);
        assert!(!gone.ok);
        assert_eq!(
            gone.error.as_deref(),
            Some("Discord does not know this webhook (deleted?)")
        );
        assert_eq!(
            LastResult::new("pool", Some(429), None, 0, 1)
                .error
                .as_deref(),
            Some("rate limited by Discord")
        );
        assert_eq!(
            LastResult::new("pool", Some(500), None, 0, 1)
                .error
                .as_deref(),
            Some("HTTP 500")
        );
        assert_eq!(
            LastResult::new("crash", None, Some("ESP_ERR_HTTP_CONNECT".into()), 0, 1)
                .error
                .as_deref(),
            Some("ESP_ERR_HTTP_CONNECT")
        );
        assert_eq!(
            LastResult::new("crash", None, None, 0, 1).error.as_deref(),
            Some("no connection")
        );
    }
}

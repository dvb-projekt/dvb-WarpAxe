//! The firmware writer's hand-off.
//!
//! Writing an app image to flash needs a task whose stack sits in
//! internal RAM (the cache is off while flash is written, so a stack in
//! PSRAM cannot be reached). Until v0.1.31 the firmware spawned such a
//! task for every write, with a 16 KB stack taken from the internal heap
//! at that moment; after hours of mining that heap is fragmented, and a
//! NerdQAxe++ with 45 KB free had no 16 KB piece left, so no update could
//! start until somebody restarted it (2026-10-20). Now the write runs on
//! the main task, whose internal stack exists from boot and is never
//! freed, and the receivers only hand the image over.
//!
//! This crate holds the state of that hand-off, apart from any locking
//! and I/O, so the rules can be tested on the host:
//!
//! - one job at a time: a job offered while another is queued or being
//!   written is refused, and the caller says so to the user instead of
//!   dropping it silently;
//! - the writer takes the queued job and stays busy until it reports
//!   `done`, whatever the outcome (a successful write ends in a restart
//!   and never reports);
//! - `done` without a job under way is harmless.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

/// Where the writer stands.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Phase {
    /// Nothing queued, nothing being written.
    Idle,
    /// A job waits for the writer.
    Queued,
    /// The writer took the job and has not reported back.
    Writing,
}

/// Why an offer was refused.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Refused {
    /// A job is queued and not yet taken.
    Queued,
    /// A job is being written.
    Writing,
}

impl Refused {
    /// What to tell the user.
    pub fn reason(self) -> &'static str {
        match self {
            Refused::Queued => "an update is already waiting to be written",
            Refused::Writing => "an update is being written right now",
        }
    }
}

/// The hand-off of one job of type `J` (the image and what to do after
/// it is written). The caller keeps it behind a lock.
#[derive(Debug)]
pub struct Handoff<J> {
    queued: Option<J>,
    writing: bool,
}

impl<J> Default for Handoff<J> {
    fn default() -> Self {
        Self {
            queued: None,
            writing: false,
        }
    }
}

impl<J> Handoff<J> {
    /// An empty hand-off.
    pub const fn new() -> Self {
        Self {
            queued: None,
            writing: false,
        }
    }

    /// Where the writer stands.
    pub fn phase(&self) -> Phase {
        if self.writing {
            Phase::Writing
        } else if self.queued.is_some() {
            Phase::Queued
        } else {
            Phase::Idle
        }
    }

    /// Queue `job` for the writer; refused with the job handed back
    /// while another is queued or being written.
    pub fn offer(&mut self, job: J) -> Result<(), (Refused, J)> {
        match self.phase() {
            Phase::Idle => {
                self.queued = Some(job);
                Ok(())
            }
            Phase::Queued => Err((Refused::Queued, job)),
            Phase::Writing => Err((Refused::Writing, job)),
        }
    }

    /// The writer takes the queued job, if there is one, and is busy
    /// until [`done`](Self::done).
    pub fn take(&mut self) -> Option<J> {
        let job = self.queued.take()?;
        self.writing = true;
        Some(job)
    }

    /// The writer is free again (a failed write; a successful one
    /// restarts the miner instead).
    pub fn done(&mut self) {
        self.writing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_job_goes_through() {
        let mut h = Handoff::<u8>::new();
        assert_eq!(h.phase(), Phase::Idle);
        assert!(h.take().is_none());
        assert_eq!(h.offer(1), Ok(()));
        assert_eq!(h.phase(), Phase::Queued);
        assert_eq!(h.take(), Some(1));
        assert_eq!(h.phase(), Phase::Writing);
        assert!(h.take().is_none(), "the same job is never taken twice");
        h.done();
        assert_eq!(h.phase(), Phase::Idle);
    }

    #[test]
    fn a_second_job_is_refused_while_one_waits() {
        let mut h = Handoff::<u8>::new();
        h.offer(1).unwrap();
        assert_eq!(h.offer(2), Err((Refused::Queued, 2)));
        assert_eq!(h.take(), Some(1), "the first job stays queued");
    }

    #[test]
    fn a_second_job_is_refused_while_one_is_written() {
        let mut h = Handoff::<u8>::new();
        h.offer(1).unwrap();
        h.take();
        assert_eq!(h.offer(2), Err((Refused::Writing, 2)));
        assert!(h.take().is_none());
        h.done();
        assert_eq!(h.offer(2), Ok(()), "free again after the writer reports");
        assert_eq!(h.take(), Some(2));
    }

    #[test]
    fn done_without_a_job_changes_nothing() {
        let mut h = Handoff::<u8>::new();
        h.done();
        assert_eq!(h.phase(), Phase::Idle);
        h.offer(3).unwrap();
        h.done();
        assert_eq!(h.phase(), Phase::Queued, "a queued job is not lost");
        assert_eq!(h.take(), Some(3));
    }

    #[test]
    fn reasons_name_the_state() {
        assert!(Refused::Queued.reason().contains("waiting"));
        assert!(Refused::Writing.reason().contains("written"));
    }
}

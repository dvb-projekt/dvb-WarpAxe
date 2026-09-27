//! The two log-ring calls the log page makes, with sample lines.
pub mod sys {
    pub mod tusb {
        const LINES: &[&str] = &[
            "I (4213) warpaxe::link: stratum: connecting to pool.warppool.org:3334 (TLS)",
            "I (4890) esp-x509-crt-bundle: Certificate validated",
            "I (5012) warpaxe::link: stratum: TLS up, certificate verified for pool.warppool.org",
            "I (5120) warpaxe::stratum: stratum: version rolling mask 1fffe000",
            "I (5188) warpaxe::stratum: stratum: difficulty 21781.97",
            "I (5241) warpaxe::stratum: stratum: authorized as bc1q...NerdQaxe++",
            "I (9020) warpaxe::miner: miner: chain ready, 4 chip(s)",
            "I (9100) warpaxe::miner: miner: clock 800 MHz",
            "W (61200) warpaxe::stratum: stratum: share 131 rejected: stale",
            "I (62100) warpaxe::stratum: stratum: job j1a4 ntime 6ab9377a clean true",
            "I (63120) warpaxe::board: board: core 1250 mV",
        ];
        /// # Safety
        /// Host stub, always safe.
        pub unsafe fn wa_log_count() -> i32 {
            LINES.len() as i32
        }
        /// # Safety
        /// `buf` must hold `len` bytes.
        pub unsafe fn wa_log_line(i: i32, buf: *mut core::ffi::c_char, len: usize) {
            let s = LINES.get(i as usize).copied().unwrap_or("");
            let n = s.len().min(len - 1);
            core::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
            *buf.add(n) = 0;
        }
    }
}

//! TLS server verification against the pinned CA bundle, with the two
//! rules ESP-IDF's verifier lacks (`warpaxe_trust`):
//!
//! - a root of the bundle that the server sends along (cross-signed by a
//!   root the bundle no longer has) is that root — for every TLS client
//!   of the firmware: the pool (`link`), the online update and the price
//!   fetch attach through [`attach`];
//! - for the pool connection only ([`attach_pool`]): a leaf certificate
//!   no public authority issued is accepted when its public key is the
//!   one the owner confirmed on the Pool page (`warpaxe_trust::pin`).
//!   The update and the price fetch never see a pin.
//!
//! Not covered: a server that also sends the old cross-signer on top of
//! the bundle's root. The callback sees the top certificate first, and
//! ESP-IDF's lookup fails there before the root below is reached (no such
//! chain seen so far; api.coingecko.com sends the root as the top).
//!
//! `esp_crt_verify_callback` is declared here by hand, as ESP-IDF's own
//! tests do. On an ESP-IDF upgrade check that its signature is unchanged,
//! and drop the sent-root part once ESP-IDF trusts a sent root itself.
//!
//! Threads: the callbacks run on the thread that handshakes (the stratum
//! thread for the pool), and only ever write RAM ([`POOL`]).

use core::ffi::{c_int, c_void};
use std::sync::Mutex;

use esp_idf_svc::sys;
use warpaxe_trust::pin::{Pin, Verdict};

extern "C" {
    /// ESP-IDF's own verify callback (`esp_crt_bundle.c`, not in its
    /// header): looks up the issuer of the topmost certificate.
    fn esp_crt_verify_callback(
        buf: *mut c_void,
        crt: *mut sys::mbedtls_x509_crt,
        depth: c_int,
        flags: *mut u32,
    ) -> c_int;
    #[link_name = "_binary_x509_crt_bundle_start"]
    static BUNDLE_START: [u8; 0];
    #[link_name = "_binary_x509_crt_bundle_end"]
    static BUNDLE_END: [u8; 0];
}

/// The bundle embedded in the image, as `esp_crt_bundle_attach` reads it.
fn bundle() -> Option<warpaxe_trust::Bundle<'static>> {
    // SAFETY: the linker places the generated bundle between the two
    // symbols, read-only, for the life of the program.
    let bytes = unsafe {
        let start = core::ptr::addr_of!(BUNDLE_START).cast::<u8>();
        let end = core::ptr::addr_of!(BUNDLE_END).cast::<u8>();
        core::slice::from_raw_parts(start, end as usize - start as usize)
    };
    warpaxe_trust::Bundle::new(bytes)
}

/// The raw bytes of an mbedTLS buffer.
///
/// # Safety
/// `b` comes from a parsed certificate that outlives the slice.
unsafe fn raw(b: &sys::mbedtls_x509_buf) -> &[u8] {
    if b.p.is_null() {
        &[]
    } else {
        core::slice::from_raw_parts(b.p, b.len)
    }
}

/// The sent-root rule, for every client. True when the certificate was
/// accepted and `flags` cleared.
///
/// # Safety
/// `c` is the certificate mbedTLS hands to the verify callback.
unsafe fn sent_root(c: &sys::mbedtls_x509_crt, depth: c_int, flags: &mut u32) -> bool {
    if let Some(b) = bundle() {
        if warpaxe_trust::is_sent_root(&b, *flags, raw(&c.subject_raw), raw(&c.pk_raw)) {
            log::info!("tls: the server sent a root of the bundle (depth {depth}): trusted");
            *flags = 0;
            return true;
        }
    }
    false
}

unsafe extern "C" fn verify(
    buf: *mut c_void,
    crt: *mut sys::mbedtls_x509_crt,
    depth: c_int,
    flags: *mut u32,
) -> c_int {
    if !crt.is_null() && !flags.is_null() && sent_root(&*crt, depth, &mut *flags) {
        return 0;
    }
    esp_crt_verify_callback(buf, crt, depth, flags)
}

/// `crt_bundle_attach` for esp-tls and the HTTP client: ESP-IDF's
/// bundle, then this module's verify callback in place of ESP-IDF's.
///
/// # Safety
/// Called by esp-tls with its `mbedtls_ssl_config`.
pub unsafe extern "C" fn attach(conf: *mut c_void) -> sys::esp_err_t {
    let ret = sys::esp_crt_bundle_attach(conf);
    if ret == sys::ESP_OK && !conf.is_null() {
        sys::mbedtls_ssl_conf_verify(
            conf.cast::<sys::mbedtls_ssl_config>(),
            Some(verify),
            core::ptr::null_mut(),
        );
    }
    ret
}

// ---------------------------------------------------------------------
// The pool connection: the owner's pin
// ---------------------------------------------------------------------

/// What one pool handshake was told and what it saw. One pool handshake
/// runs at a time (the stratum thread's), so one slot is enough; `begin`
/// sets it up and `outcome` reads it back after the handshake.
#[derive(Default, Clone)]
struct PoolTrust {
    /// The pin stored for this pool, if any.
    pin: Option<Pin>,
    /// What the leaf's key turned out to be, and why it was refused.
    seen: Option<Seen>,
    /// The leaf was accepted by the pin (not by the bundle).
    pinned: bool,
}

/// A pool key the verify callback refused, kept in RAM for the Pool
/// page's confirmation.
#[derive(Clone, Copy, Debug)]
pub struct Seen {
    /// The key the pool presented.
    pub pin: Pin,
    /// A pin was stored and this is another key.
    pub changed: bool,
}

static POOL: Mutex<PoolTrust> = Mutex::new(PoolTrust {
    pin: None,
    seen: None,
    pinned: false,
});

/// Before a pool handshake: the pin to compare with, nothing seen yet.
pub fn pool_begin(pin: Option<Pin>) {
    *POOL.lock().unwrap() = PoolTrust {
        pin,
        seen: None,
        pinned: false,
    };
}

/// After the pool handshake: what the leaf's key was when it was refused
/// (`None` when the bundle or the pin accepted it, or when the handshake
/// failed before a certificate), and whether the pin did the accepting.
pub fn pool_outcome() -> (Option<Seen>, bool) {
    let p = POOL.lock().unwrap();
    (p.seen, p.pinned)
}

unsafe extern "C" fn verify_pool(
    buf: *mut c_void,
    crt: *mut sys::mbedtls_x509_crt,
    depth: c_int,
    flags: *mut u32,
) -> c_int {
    if !crt.is_null() && !flags.is_null() {
        let c = &*crt;
        if sent_root(c, depth, &mut *flags) {
            return 0;
        }
        let pin = POOL.lock().unwrap().pin;
        match warpaxe_trust::pin::verdict(depth, *flags, pin.as_ref(), raw(&c.pk_raw)) {
            Verdict::Untouched => {}
            Verdict::Pinned => {
                log::info!("tls: pool certificate accepted by the pinned key (flags {:#x})", *flags);
                *flags = 0;
                POOL.lock().unwrap().pinned = true;
                return 0;
            }
            Verdict::Unconfirmed(seen) => {
                log::warn!(
                    "tls: pool certificate not from a public authority; its key {} is not confirmed (flags {:#x})",
                    seen.text().short(),
                    *flags
                );
                POOL.lock().unwrap().seen = Some(Seen {
                    pin: seen,
                    changed: false,
                });
            }
            Verdict::Changed(seen) => {
                log::warn!(
                    "tls: the pool's key changed: {} is not the pinned key (flags {:#x})",
                    seen.text().short(),
                    *flags
                );
                POOL.lock().unwrap().seen = Some(Seen {
                    pin: seen,
                    changed: true,
                });
            }
        }
    }
    esp_crt_verify_callback(buf, crt, depth, flags)
}

/// `crt_bundle_attach` for the pool connection: as [`attach`], plus the
/// pin rule from [`pool_begin`]. Never attach this anywhere else.
///
/// # Safety
/// Called by esp-tls with its `mbedtls_ssl_config`.
pub unsafe extern "C" fn attach_pool(conf: *mut c_void) -> sys::esp_err_t {
    let ret = sys::esp_crt_bundle_attach(conf);
    if ret == sys::ESP_OK && !conf.is_null() {
        sys::mbedtls_ssl_conf_verify(
            conf.cast::<sys::mbedtls_ssl_config>(),
            Some(verify_pool),
            core::ptr::null_mut(),
        );
    }
    ret
}

//! TLS server verification against the pinned CA bundle, with the one
//! rule ESP-IDF's verifier lacks: a root of the bundle that the server
//! sends along (cross-signed by a root the bundle no longer has) is that
//! root (`warpaxe_trust`). Every TLS client of the firmware attaches
//! through [`attach`]: the pool (`link`), the online update and the price
//! fetch.

use core::ffi::{c_int, c_void};
use esp_idf_svc::sys;

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
    static BUNDLE_START: u8;
    #[link_name = "_binary_x509_crt_bundle_end"]
    static BUNDLE_END: u8;
}

/// The bundle embedded in the image, as `esp_crt_bundle_attach` reads it.
fn bundle() -> Option<warpaxe_trust::Bundle<'static>> {
    // SAFETY: the linker places the generated bundle between the two
    // symbols, read-only, for the life of the program.
    let bytes = unsafe {
        let start = core::ptr::addr_of!(BUNDLE_START);
        let end = core::ptr::addr_of!(BUNDLE_END);
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

unsafe extern "C" fn verify(
    buf: *mut c_void,
    crt: *mut sys::mbedtls_x509_crt,
    depth: c_int,
    flags: *mut u32,
) -> c_int {
    if !crt.is_null() && !flags.is_null() {
        let c = &*crt;
        if let Some(b) = bundle() {
            if warpaxe_trust::is_sent_root(&b, *flags, raw(&c.subject_raw), raw(&c.pk_raw)) {
                log::info!("tls: the server sent a root of the bundle (depth {depth}): trusted");
                *flags = 0;
                return 0;
            }
        }
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

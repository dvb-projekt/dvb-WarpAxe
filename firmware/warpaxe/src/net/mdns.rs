//! mDNS / DNS-SD, as AxeOS and NerdOS announce a miner: the hostname as
//! `<hostname>.local`, and `_http._tcp` on port 80 with the `_axeos`
//! subtype and the TXT items `board`, `family`, `asic`, `asic_count` and
//! `fw_version`, so tools that find Bitaxes that way find this one too.
//!
//! Started once the station or the Ethernet link has an address. A name already taken on the
//! network is renamed by the mDNS component itself (`name-2`);
//! [`hostname`] reports the name actually announced.

use std::sync::Mutex;

use esp_idf_svc::mdns::EspMdns;
use esp_idf_svc::sys;

use crate::board;

static MDNS: Mutex<Option<EspMdns>> = Mutex::new(None);

/// A DNS label from the configured hostname: lower case, letters, digits
/// and `-`, at most 63 bytes, never empty.
pub fn label(hostname: &str) -> String {
    let mut s: String = hostname
        .trim()
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect();
    s = s.trim_matches('-').to_string();
    s.truncate(63);
    if s.is_empty() {
        "warpaxe".into()
    } else {
        s
    }
}

/// Starts mDNS on a thread of its own once Wi-Fi or Ethernet has an
/// address. The mDNS component itself answers on every interface it
/// knows (its predefined station and `ETH_DEF` netifs), so a cable that
/// comes later is announced too.
pub fn start(hostname: String) {
    std::thread::Builder::new()
        .stack_size(4096)
        .spawn(move || {
            while super::state().lan_ip().is_none() {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            if let Err(e) = announce(&label(&hostname)) {
                log::warn!("mdns: not announced: {e}");
            }
        })
        .expect("mdns thread");
}

fn announce(host: &str) -> Result<(), sys::EspError> {
    let mut mdns = EspMdns::take()?;
    mdns.set_hostname(host)?;
    let p = board::profile();
    let (family, asic, count) = if board::devkit() {
        ("WarpMiner", "ESP32-S3 SHA", 0)
    } else {
        (p.model, board::chip().name, p.asic_count)
    };
    // AxeOS: "Bitaxe <family> <board> (<last two MAC bytes>)".
    let mac = super::usb::base_mac();
    let instance = format!("WarpAxe {} ({:02X}{:02X})", p.model, mac[4], mac[5]);
    mdns.set_instance_name(&instance)?;
    let count = count.to_string();
    let fw = env!("CARGO_PKG_VERSION");
    mdns.add_service(
        Some(&instance),
        "_http",
        "_tcp",
        80,
        &[
            ("board", p.name),
            ("family", family),
            ("asic", asic),
            ("asic_count", &count),
            ("fw_version", fw),
        ],
    )?;
    let c = |s: &str| std::ffi::CString::new(s).unwrap();
    let (inst, ty, proto, sub) = (c(&instance), c("_http"), c("_tcp"), c("_axeos"));
    sys::esp!(unsafe {
        sys::mdns_service_subtype_add_for_host(
            inst.as_ptr(),
            ty.as_ptr(),
            proto.as_ptr(),
            std::ptr::null(),
            sub.as_ptr(),
        )
    })?;
    log::info!("mdns: {host}.local, _http._tcp \"{instance}\" (_axeos)");
    *MDNS.lock().unwrap() = Some(mdns);
    Ok(())
}

/// The name announced, without `.local`: `None` until mDNS runs. After
/// a name clash it is the component's renamed one (`name-2`).
pub fn hostname() -> Option<String> {
    MDNS.lock().unwrap().as_ref()?;
    let mut buf = [0u8; 64];
    let rc = unsafe { sys::mdns_hostname_get(buf.as_mut_ptr() as *mut _) };
    if rc != sys::ESP_OK {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..end]).ok().map(str::to_string)
}

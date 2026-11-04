//! Network interfaces as a list. Nothing above this module knows or cares
//! whether a packet goes out over USB-C, Wi-Fi or the Ethernet HAT.
//!
//! Routing is lwIP's: every interface with an address is up, and the
//! default route follows the highest `route_priority` among them
//! (Ethernet 110, the Wi-Fi station 100, USB 30: `eth.rs`, `wifi.rs`,
//! `usb.rs`). So with a link on the HAT the pool, the update check and
//! the price fetch go out over Ethernet, and when the link goes they
//! move to Wi-Fi on their own; the HTTP server, mDNS and the API answer
//! on every interface all the time.

pub mod eth;
pub mod mdns;
pub mod usb;
pub mod wifi;

use std::sync::Mutex;

#[derive(Clone, Debug, Default)]
pub struct NetState {
    pub usb_mounted: bool,
    pub usb_ip: Option<String>,
    pub wifi_connected: bool,
    pub wifi_ip: Option<String>,
    pub wifi_rssi: Option<i32>,
    /// The Ethernet HAT: `None` while the board has no pins for one or
    /// the probe has not run; `Some(false)` when no W5500 answered on the
    /// board's pins (no HAT fitted); `Some(true)` when one did and the
    /// driver runs.
    pub eth_fitted: Option<bool>,
    /// The cable is in and the link negotiated.
    pub eth_link: bool,
    /// Ethernet has an address (DHCP done).
    pub eth_connected: bool,
    pub eth_ip: Option<String>,
    /// The W5500's MAC, as the driver set it (the ESP's efuse Ethernet
    /// address, `ESP_MAC_ETH`).
    pub eth_mac: Option<String>,
    /// Negotiated link speed, Mbit/s (10 or 100), while the link is up.
    pub eth_speed_mbit: Option<u16>,
    /// Negotiated full duplex, while the link is up.
    pub eth_full_duplex: Option<bool>,
    /// The probe or the driver start is still running: the setup hotspot
    /// waits for its verdict before it opens for a board without a
    /// network in the settings.
    pub eth_probing: bool,
    /// The bus, the driver or the netif failed (the log says which): a
    /// HAT that is there and does not work, or none to be told.
    pub eth_failed: bool,
}

impl NetState {
    /// The interface the default route goes out on: the one lwIP picks
    /// by route priority among those with an address.
    pub fn active(&self) -> Option<&'static str> {
        if self.eth_connected {
            Some("ethernet")
        } else if self.wifi_connected {
            Some("wifi")
        } else if self.usb_mounted {
            Some("usb")
        } else {
            None
        }
    }

    /// The address on the local network, Ethernet before Wi-Fi: what the
    /// swarm page scans from, what the display shows. `None` with only
    /// the USB link.
    pub fn lan_ip(&self) -> Option<String> {
        if self.eth_connected {
            self.eth_ip.clone()
        } else if self.wifi_connected {
            self.wifi_ip.clone()
        } else {
            None
        }
    }

    /// The address to reach the web UI at: the LAN one, else the USB one.
    pub fn ip(&self) -> Option<String> {
        self.lan_ip().or_else(|| self.usb_ip.clone())
    }
}

static STATE: Mutex<NetState> = Mutex::new(NetState {
    usb_mounted: false,
    usb_ip: None,
    wifi_connected: false,
    wifi_ip: None,
    wifi_rssi: None,
    eth_fitted: None,
    eth_link: false,
    eth_connected: false,
    eth_ip: None,
    eth_mac: None,
    eth_speed_mbit: None,
    eth_full_duplex: None,
    eth_probing: false,
    eth_failed: false,
});

pub fn state() -> NetState {
    let mut s = STATE.lock().unwrap().clone();
    s.usb_mounted = usb::mounted();
    s.usb_ip = usb::mounted().then(|| usb::ip_string());
    s
}

pub(crate) fn set_wifi(connected: bool, ip: Option<String>, rssi: Option<i32>) {
    let mut s = STATE.lock().unwrap();
    s.wifi_connected = connected;
    s.wifi_ip = ip;
    s.wifi_rssi = rssi;
}

/// What the Ethernet thread found and sees ([`eth`]).
pub(crate) fn set_eth(f: impl FnOnce(&mut NetState)) {
    f(&mut STATE.lock().unwrap());
}

/// The setup hotspot while it is up: its name and password, for the
/// device display only (never in the API).
static HOTSPOT: Mutex<Option<(String, String)>> = Mutex::new(None);

pub fn hotspot() -> Option<(String, String)> {
    HOTSPOT.lock().unwrap().clone()
}

pub(crate) fn set_hotspot(h: Option<(String, String)>) {
    *HOTSPOT.lock().unwrap() = h;
}

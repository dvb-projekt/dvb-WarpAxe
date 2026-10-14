//! Network interfaces as a list. Nothing above this module knows or cares
//! whether a packet goes out over USB-C or Wi-Fi.

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
}

static STATE: Mutex<NetState> = Mutex::new(NetState {
    usb_mounted: false,
    usb_ip: None,
    wifi_connected: false,
    wifi_ip: None,
    wifi_rssi: None,
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

/// The setup hotspot while it is up: its name and password, for the
/// device display only (never in the API).
static HOTSPOT: Mutex<Option<(String, String)>> = Mutex::new(None);

pub fn hotspot() -> Option<(String, String)> {
    HOTSPOT.lock().unwrap().clone()
}

pub(crate) fn set_hotspot(h: Option<(String, String)>) {
    *HOTSPOT.lock().unwrap() = h;
}

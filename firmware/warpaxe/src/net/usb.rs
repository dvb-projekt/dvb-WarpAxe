//! USB-C as a network interface: TinyUSB CDC-NCM (plus a CDC-ACM port),
//! an `esp_netif` with a DHCP server on 192.168.7.1/24. Proven on the
//! T-Display-S3 (firmware/proof-usb); the lessons are in the comments.

use core::ffi::c_void;
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use esp_idf_svc::sys::{self as sys, tusb, EspError};

pub const IP: [u8; 4] = [192, 168, 7, 1];

static mut NETIF: *mut sys::esp_netif_obj = null_mut();
pub static RX_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static TX_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static TX_DROPPED: AtomicU32 = AtomicU32::new(0);
/// The host has sent at least one frame: its NCM data interface is open.
/// TinyUSB's NCM driver never calls the network init callback, so this is
/// the only reliable sign.
static SEEN_RX: AtomicBool = AtomicBool::new(false);

pub fn ip_string() -> String {
    format!("{}.{}.{}.{}", IP[0], IP[1], IP[2], IP[3])
}

pub fn base_mac() -> [u8; 6] {
    let mut m = [0u8; 6];
    unsafe {
        sys::esp_read_mac(m.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_WIFI_STA);
    }
    m
}

unsafe extern "C" fn on_recv(buffer: *mut c_void, len: u16, _ctx: *mut c_void) -> sys::esp_err_t {
    SEEN_RX.store(true, Ordering::Relaxed);
    let netif = NETIF;
    if netif.is_null() {
        return sys::ESP_OK;
    }
    let copy = sys::malloc(u32::from(len));
    if copy.is_null() {
        return sys::ESP_ERR_NO_MEM;
    }
    core::ptr::copy_nonoverlapping(buffer as *const u8, copy as *mut u8, usize::from(len));
    RX_FRAMES.fetch_add(1, Ordering::Relaxed);
    sys::esp_netif_receive(netif, copy, usize::from(len), copy)
}

unsafe extern "C" fn transmit(_h: *mut c_void, buffer: *mut c_void, len: usize) -> sys::esp_err_t {
    // No logging here: this runs on the tcpip thread.
    if !link_ready() {
        TX_DROPPED.fetch_add(1, Ordering::Relaxed);
        return sys::ESP_OK;
    }
    if tusb::wa_usb_send(buffer, len as u16) == sys::ESP_OK {
        TX_FRAMES.fetch_add(1, Ordering::Relaxed);
    }
    sys::ESP_OK
}

/// The netstack config we borrow (Wi-Fi AP) calls the wrap variant; a
/// NULL here is a jump to nowhere on the first gratuitous ARP.
unsafe extern "C" fn transmit_wrap(
    h: *mut c_void,
    buffer: *mut c_void,
    len: usize,
    _netstack_buf: *mut c_void,
) -> sys::esp_err_t {
    transmit(h, buffer, len)
}

unsafe extern "C" fn free_rx(_h: *mut c_void, buffer: *mut c_void) {
    sys::free(buffer);
}

fn ip4(b: [u8; 4]) -> sys::esp_ip4_addr {
    sys::esp_ip4_addr {
        addr: u32::from_ne_bytes(b),
    }
}

/// Install TinyUSB (NCM + CDC from Kconfig) and start the NCM link.
pub fn start_usb() -> Result<(), EspError> {
    let mut host_mac = base_mac();
    host_mac[5] ^= 1;
    unsafe {
        sys::esp!(tusb::wa_usb_start(
            host_mac.as_ptr(),
            Some(on_recv),
            null_mut()
        ))
    }
}

/// The netif with DHCP server behind the NCM link. `esp_netif_init` and
/// the default event loop must already exist.
pub fn start_netif() -> Result<(), EspError> {
    let mac = base_mac();
    unsafe {
        let ip_info: &'static sys::esp_netif_ip_info_t =
            Box::leak(Box::new(sys::esp_netif_ip_info_t {
                ip: ip4(IP),
                netmask: ip4([255, 255, 255, 0]),
                gw: ip4(IP),
            }));
        let mut base: sys::esp_netif_inherent_config_t = core::mem::zeroed();
        base.flags =
            sys::esp_netif_flags_ESP_NETIF_DHCP_SERVER | sys::esp_netif_flags_ESP_NETIF_FLAG_AUTOUP;
        base.mac = mac;
        base.ip_info = ip_info;
        base.if_key = c"usb".as_ptr();
        base.if_desc = c"usb".as_ptr();
        base.route_prio = 30;
        let cfg = sys::esp_netif_config_t {
            base: &base,
            driver: null(),
            stack: sys::_g_esp_netif_netstack_default_wifi_ap,
        };
        let netif = sys::esp_netif_new(&cfg);
        if netif.is_null() {
            return Err(EspError::from_infallible::<{ sys::ESP_ERR_NO_MEM }>());
        }
        let drv = sys::esp_netif_driver_ifconfig_t {
            handle: netif as *mut c_void,
            transmit: Some(transmit),
            transmit_wrap: Some(transmit_wrap),
            driver_free_rx_buffer: Some(free_rx),
        };
        sys::esp!(sys::esp_netif_set_driver_config(netif, &drv))?;
        NETIF = netif;
        sys::esp_netif_action_start(netif as *mut c_void, null_mut(), 0, null_mut());
        sys::esp_netif_action_connected(netif as *mut c_void, null_mut(), 0, null_mut());
    }
    Ok(())
}

pub fn mounted() -> bool {
    unsafe { tusb::wa_usb_mounted() }
}

pub fn link_ready() -> bool {
    SEEN_RX.load(Ordering::Relaxed) && mounted()
}

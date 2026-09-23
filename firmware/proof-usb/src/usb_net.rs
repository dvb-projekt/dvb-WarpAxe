//! USB-C as a network interface: TinyUSB in CDC-NCM mode (plus a CDC-ACM
//! console), glued to an `esp_netif` that runs a DHCP server on
//! 192.168.7.1/24. The host sees a USB Ethernet adapter and gets an
//! address; the board is reachable at 192.168.7.1 with no Wi-Fi at all.
//!
//! The glue follows the IDF example `tusb_ncm` (which bridges the other
//! way, to Wi-Fi): TinyUSB's receive callback hands frames to
//! `esp_netif_receive`, the netif's transmit hook hands frames to
//! `tinyusb_net_send_sync`.

use core::ffi::c_void;
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicU32, Ordering};

use esp_idf_svc::sys::{self as sys, tusb, EspError};

pub const IP: [u8; 4] = [192, 168, 7, 1];

static mut NETIF: *mut sys::esp_netif_obj = null_mut();
pub static RX_FRAMES: AtomicU32 = AtomicU32::new(0);
pub static TX_FRAMES: AtomicU32 = AtomicU32::new(0);

pub fn base_mac() -> [u8; 6] {
    let mut m = [0u8; 6];
    unsafe {
        sys::esp_read_mac(m.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_WIFI_STA);
    }
    m
}

unsafe extern "C" fn on_recv(buffer: *mut c_void, len: u16, _ctx: *mut c_void) -> sys::esp_err_t {
    let netif = NETIF;
    if netif.is_null() {
        return sys::ESP_OK;
    }
    // TinyUSB owns `buffer` only until we return: copy it out.
    let copy = sys::malloc(u32::from(len));
    if copy.is_null() {
        return sys::ESP_ERR_NO_MEM;
    }
    core::ptr::copy_nonoverlapping(buffer as *const u8, copy as *mut u8, usize::from(len));
    RX_FRAMES.fetch_add(1, Ordering::Relaxed);
    sys::esp_netif_receive(netif, copy, usize::from(len), copy)
}

unsafe extern "C" fn transmit(_h: *mut c_void, buffer: *mut c_void, len: usize) -> sys::esp_err_t {
    TX_FRAMES.fetch_add(1, Ordering::Relaxed);
    let rc = tusb::wa_usb_send(buffer, len as u16);
    if rc != sys::ESP_OK {
        log::warn!("usb tx failed: {rc}");
    }
    sys::ESP_OK
}

unsafe extern "C" fn free_rx(_h: *mut c_void, buffer: *mut c_void) {
    sys::free(buffer);
}

fn ip4(b: [u8; 4]) -> sys::esp_ip4_addr {
    sys::esp_ip4_addr {
        addr: u32::from_ne_bytes(b),
    }
}

/// Bring up TinyUSB (NCM + CDC-ACM from Kconfig), move the console to the
/// CDC port, and attach the NCM link to a netif with a DHCP server.
pub fn start_usb(mac: [u8; 6]) -> Result<(), EspError> {
    // The MAC the host's adapter reports is ours with the last bit
    // flipped; the netif keeps the base MAC. Two ends, two addresses.
    let mut host_mac = mac;
    host_mac[5] ^= 1;
    unsafe { sys::esp!(tusb::wa_usb_start(host_mac.as_ptr(), Some(on_recv), null_mut())) }
}

/// The netif with DHCP server behind the NCM link.
pub fn start_netif(mac: [u8; 6]) -> Result<(), EspError> {
    unsafe {
        sys::esp!(sys::esp_netif_init())?;
        sys::esp!(sys::esp_event_loop_create_default())?;

        let ip_info: &'static sys::esp_netif_ip_info_t = Box::leak(Box::new(sys::esp_netif_ip_info_t {
            ip: ip4(IP),
            netmask: ip4([255, 255, 255, 0]),
            gw: ip4(IP),
        }));
        let mut base: sys::esp_netif_inherent_config_t = core::mem::zeroed();
        base.flags = sys::esp_netif_flags_ESP_NETIF_DHCP_SERVER
            | sys::esp_netif_flags_ESP_NETIF_FLAG_AUTOUP;
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
            transmit_wrap: None,
            driver_free_rx_buffer: Some(free_rx),
        };
        sys::esp!(sys::esp_netif_set_driver_config(netif, &drv))?;
        NETIF = netif;
        sys::esp_netif_action_start(netif as *mut c_void, null_mut(), 0, null_mut());
        sys::esp_netif_action_connected(netif as *mut c_void, null_mut(), 0, null_mut());
    }
    Ok(())
}

/// Enumerated and configured by a host.
pub fn mounted() -> bool {
    unsafe { tusb::wa_usb_mounted() }
}

/// Reboot into the ROM download mode: the ROM re-muxes the USB PHY to
/// USB-Serial/JTAG, so `espflash`/`esptool` can flash without anyone
/// touching the BOOT button. This is the flash-back path that must work
/// before this firmware ever meets a miner.
pub fn reboot_to_bootloader() -> ! {
    unsafe { tusb::wa_reboot_to_bootloader() }
}

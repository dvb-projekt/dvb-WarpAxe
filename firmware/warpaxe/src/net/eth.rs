//! Ethernet over the W5500 HAT ("NerdHat", Bitronics NERDETH rev 1.1) on
//! the Nerd boards that have a header for it (`Profile::ethernet`,
//! docs/13). After NerdOS v1.1.1's `main/network/w5500.cpp` (PRs #724
//! and #731): the chip is looked for at boot by reading its VERSIONR
//! register over SPI, reset over SPI (the HAT has no reset line, its RC
//! network works at power-on only, and a software restart of the ESP
//! must reset the W5500 too), and only then is the ESP-IDF driver
//! installed. Without a HAT nothing on the pins is driven beyond that
//! one read, and the firmware runs as before on Wi-Fi and USB.
//!
//! The link is one more interface in `net::state()`. Its netif has a
//! higher route priority than the Wi-Fi station, so with a cable the
//! pool, the update check and the price fetch leave over Ethernet and
//! Wi-Fi stays as the fallback; lwIP moves the default route when the
//! link goes and comes back. A pool session already running over Wi-Fi
//! is moved to Ethernet once the link has held its address for a while
//! ([`SETTLE`]), never on every flap of the cable.

use std::time::{Duration, Instant};

use esp_idf_svc::eth::{EspEth, EthDriver, SpiEthChipset, SpiEventSource};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{AnyIOPin, AnyInputPin, AnyOutputPin};
use esp_idf_svc::hal::spi::{
    config::{Config as SpiConfig, DriverConfig},
    Dma, SpiDeviceDriver, SpiDriver, SPI2,
};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::ipv4;
use esp_idf_svc::netif::{EspNetif, NetifConfiguration};
use esp_idf_svc::sys;
use warpaxe_board::EthPins;

use super::static_ip::{self, Guard, Step};

/// SPI clock, as NerdOS runs the HAT (long wires on a header).
const SPI_HZ: u32 = 2_000_000;
/// Without an interrupt line the driver asks the chip this often.
/// NerdOS polls every 1 ms; a miner's traffic is a few KB a second, the
/// chip buffers 16 KB of frames and the receive task drains them all on
/// each wake-up, and the 2 MHz SPI bus (about 250 KB/s) is the limit
/// either way: 5 ms wakes the driver a fifth as often.
const POLL: Duration = Duration::from_millis(5);
/// W5500 common-register addresses and the control byte: block select
/// 0 (common), read 0x00 / write 0x04, variable data length.
const REG_MR: u16 = 0x0000;
const REG_VERSIONR: u16 = 0x0039;
const CTRL_READ: u8 = 0x00;
const CTRL_WRITE: u8 = 0x04;
/// MR.RST: the chip resets itself and clears the bit within a millisecond.
const MR_RST: u8 = 0x80;
/// What VERSIONR reads on every W5500.
const VERSION: u8 = 0x04;
/// Above the Wi-Fi station's 100 (`NetifConfiguration::wifi_default_client`)
/// and the USB netif's 30: with an address here the default route is the
/// cable.
const ROUTE_PRIORITY: u32 = 110;
/// A link without an address for this long (a port without a DHCP
/// server, a DHCP outage) and the driver is stopped and started again:
/// esp_netif keeps the default route on a netif that is up, address or
/// not, so a link that never gets a lease would otherwise hold the route
/// away from a working Wi-Fi after one Wi-Fi flap. Stopping takes the
/// netif down (the route moves), starting retries DHCP.
pub const NO_ADDRESS: Duration = Duration::from_secs(60);
/// Ethernet holds an address this long before a pool session that runs
/// over Wi-Fi is moved onto it. A cable that comes and goes within the
/// time causes no reconnect of its own; the session that was on the
/// cable ends with the link anyway (lwIP aborts its socket with the
/// address), and the next one leaves over Wi-Fi.
const SETTLE: Duration = Duration::from_secs(30);

/// Look for the HAT on `pins` and run the link on its own thread. Called
/// once `esp_netif_init` has run and the board is known; the caller has
/// made sure the pins are free (`board::ethernet_pins`).
///
/// The thread's stack lives in PSRAM: it writes no flash, and its 8 KB
/// in the internal RAM, next to the W5500 driver's own receive task,
/// left the .41 with 832 bytes as its largest free internal block
/// (mDNS, DNS, I2C and the web server then failed their allocations).
pub fn start(spi: SPI2, sysloop: EspSystemEventLoop, pins: EthPins, hostname: String) {
    super::set_eth(|s| s.eth_probing = true);
    if let Err(e) = crate::diag::spawn_psram("eth", 8192, move || {
        run(spi, sysloop, pins, hostname);
        super::set_eth(|s| s.eth_probing = false);
    }) {
        log::error!("eth: thread failed: {e}");
        super::set_eth(|s| {
            s.eth_probing = false;
            s.eth_failed = true;
        });
    }
}

/// The ESP's efuse Ethernet address (ESP_MAC_ETH): distinct from the
/// Wi-Fi station's, the soft AP's and the USB link's. ESP-IDF sets it on
/// its own only for the internal EMAC; a SPI MAC like the W5500 keeps
/// 00:00:00:00:00:00 unless it is written, and with its MAC filter on it
/// would then drop every unicast frame.
fn eth_mac() -> [u8; 6] {
    let mut m = [0u8; 6];
    // SAFETY: 6-byte out-buffer as the call requires.
    unsafe {
        sys::esp_read_mac(m.as_mut_ptr(), sys::esp_mac_type_t_ESP_MAC_ETH);
    }
    m
}

fn mac_text(m: &[u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

/// One W5500 register access on a device of its own on the bus, at the
/// probe clock: the frame is the 16-bit address, the control byte, then
/// one data byte (sent, or clocked in while a zero goes out).
fn reg<'a>(
    dev: &mut SpiDeviceDriver<'a, &'a SpiDriver<'a>>,
    addr: u16,
    ctrl: u8,
    data: u8,
) -> Result<u8, sys::EspError> {
    let mut frame = [(addr >> 8) as u8, addr as u8, ctrl, data];
    dev.transfer_in_place(&mut frame)?;
    Ok(frame[3])
}

fn run(spi: SPI2, sysloop: EspSystemEventLoop, pins: EthPins, hostname: String) {
    // SAFETY: the profile's HAT pins are used by nothing else on that
    // board (`profile::tests::the_nerdhat_pins_touch_nothing_the_board_drives`),
    // and `board::ethernet_pins` has ruled out the shared-pin boards.
    let (sclk, mosi, miso, cs) = unsafe {
        (
            AnyIOPin::new(i32::from(pins.sclk)),
            AnyIOPin::new(i32::from(pins.mosi)),
            AnyIOPin::new(i32::from(pins.miso)),
            AnyOutputPin::new(i32::from(pins.cs)),
        )
    };
    // DMA for the frames the driver moves (up to 1.5 KB in one transfer).
    let bus = match SpiDriver::new(
        spi,
        sclk,
        mosi,
        Some(miso),
        &DriverConfig::new().dma(Dma::Auto(4096)),
    ) {
        Ok(b) => b,
        Err(e) => {
            log::error!("eth: spi bus failed: {e}");
            super::set_eth(|s| s.eth_failed = true);
            return;
        }
    };
    // The probe: VERSIONR must read 0x04. A board without the HAT reads
    // 0xFF or 0x00 (MISO floating or pulled), and that is the end of it.
    let version = {
        let cfg = SpiConfig::new().baudrate(Hertz(SPI_HZ));
        let mut dev = match SpiDeviceDriver::new(&bus, Some(cs), &cfg) {
            Ok(d) => d,
            Err(e) => {
                log::error!("eth: spi device failed: {e}");
                super::set_eth(|s| s.eth_failed = true);
                return;
            }
        };
        match reg(&mut dev, REG_VERSIONR, CTRL_READ, 0) {
            Ok(VERSION) => {
                // The software reset NerdOS sends (the HAT has no reset
                // line): a restart of the ESP alone leaves the chip as it
                // was. The driver below resets and checks it once more.
                if let Err(e) = reg(&mut dev, REG_MR, CTRL_WRITE, MR_RST) {
                    log::warn!("eth: software reset failed: {e}");
                }
                std::thread::sleep(Duration::from_millis(2));
                reg(&mut dev, REG_VERSIONR, CTRL_READ, 0).unwrap_or(0)
            }
            Ok(v) => v,
            Err(e) => {
                log::warn!("eth: probe failed: {e}");
                0
            }
        }
    };
    let present = version == VERSION;
    super::set_eth(|s| s.eth_fitted = Some(present));
    if !present {
        log::info!(
            "eth: no W5500 on SCLK {} / MOSI {} / MISO {} / CS {} (VERSIONR {version:#04x}, a W5500 reads 0x04): Wi-Fi and USB only",
            pins.sclk,
            pins.mosi,
            pins.miso,
            pins.cs
        );
        return;
    }
    log::info!(
        "eth: W5500 found on SCLK {} / MOSI {} / MISO {} / CS {}, reset over SPI; {}",
        pins.sclk,
        pins.mosi,
        pins.miso,
        pins.cs,
        match pins.int {
            Some(p) => format!("interrupt on GPIO{p}"),
            None => format!("polled every {} ms", POLL.as_millis()),
        }
    );
    // SAFETY: as above; the chip select once more for the driver's own
    // device on the bus.
    let cs = unsafe { AnyOutputPin::new(i32::from(pins.cs)) };
    let rst = pins.rst.map(|p| unsafe { AnyOutputPin::new(i32::from(p)) });
    let event = match pins.int {
        Some(p) => SpiEventSource::interrupt(unsafe { AnyInputPin::new(i32::from(p)) }),
        None => SpiEventSource::polling(POLL).expect("poll period"),
    };
    let mac = eth_mac();
    let driver = match EthDriver::new_spi_with_event_source(
        bus,
        event,
        Some(cs),
        rst,
        SpiEthChipset::W5500,
        Hertz(SPI_HZ),
        Some(&mac),
        None,
        sysloop,
    ) {
        Ok(d) => d,
        Err(e) => {
            log::error!("eth: driver failed: {e}");
            super::set_eth(|s| s.eth_failed = true);
            return;
        }
    };
    let netif_conf = NetifConfiguration {
        ip_configuration: Some(ipv4::Configuration::Client(
            ipv4::ClientConfiguration::DHCP(ipv4::DHCPClientSettings {
                hostname: heapless::String::try_from(hostname.as_str()).ok(),
            }),
        )),
        route_priority: ROUTE_PRIORITY,
        ..NetifConfiguration::eth_default_client()
    };
    let mut eth =
        match EspNetif::new_with_conf(&netif_conf).and_then(|n| EspEth::wrap_all(driver, n)) {
            Ok(e) => e,
            Err(e) => {
                log::error!("eth: netif failed: {e}");
                super::set_eth(|s| s.eth_failed = true);
                return;
            }
        };
    // The static IPv4 address (`net::static_ip`) goes to the cable when
    // the setting is on: set before each driver start, since a stop puts
    // the netif back on DHCP.
    let mut guard = static_ip::planned()
        .map(|s| Guard::new(s, "ethernet"))
        .filter(|g| static_ip::apply(eth.netif(), g.settings(), "ethernet"));
    if let Err(e) = eth.start() {
        log::error!("eth: start failed: {e}");
        super::set_eth(|s| s.eth_failed = true);
        return;
    }
    // What the netif carries after the glue copied the driver's address:
    // the one written above, unless something is off.
    let netif_mac = eth.netif().get_mac().unwrap_or(mac);
    if netif_mac != mac {
        log::warn!(
            "eth: the netif carries {} instead of {}",
            mac_text(&netif_mac),
            mac_text(&mac)
        );
    }
    log::info!("eth: started, MAC {}", mac_text(&netif_mac));
    super::set_eth(|s| {
        s.eth_mac = Some(mac_text(&netif_mac));
        s.eth_probing = false;
    });

    let mut was_link = false;
    let mut ip: Option<String> = None;
    // When the address came, for the move of a Wi-Fi pool session.
    let mut ip_since: Option<Instant> = None;
    let mut moved = false;
    // When the link came up without an address yet (NO_ADDRESS).
    let mut link_since: Option<Instant> = None;
    // A link went NO_ADDRESS without an address (no DHCP server on the
    // cable): the setup hotspot no longer waits for it.
    let mut dhcp_failed = false;
    // Since when the cable has been waiting for an address, across link
    // flaps (which restart `link_since`): a link that keeps dropping must
    // not hold the hotspot back for good either.
    let mut waiting_since: Option<Instant> = None;
    // Since when the link has been down: a cable out for a while starts
    // its wait for an address afresh.
    let mut down_since: Option<Instant> = None;
    loop {
        let link = eth.is_connected().unwrap_or(false);
        let now_ip = if link {
            eth.netif()
                .get_ip_info()
                .ok()
                .filter(|i| !i.ip.is_unspecified())
                .map(|i| i.ip.to_string())
        } else {
            None
        };
        if link != was_link {
            if link {
                log::info!("eth: link up{}", link_text(&eth));
                link_since = Some(Instant::now());
            } else {
                log::warn!("eth: link down");
                link_since = None;
            }
            was_link = link;
        }
        if now_ip != ip {
            match &now_ip {
                Some(a) => {
                    log::info!("eth: address {a}, the default route is the cable");
                    super::set_ip4(true, super::Ip4::of(eth.netif()));
                    ip_since = Some(Instant::now());
                    moved = false;
                }
                None => {
                    log::warn!("eth: address gone, the default route is Wi-Fi (if up)");
                    ip_since = None;
                    super::set_ip4(true, None);
                    link_since = link.then(Instant::now);
                    // A pool session still up on the cable's address is
                    // dead with it: end it now rather than wait for its
                    // read timeout. Usually lwIP has aborted it already
                    // (the address change) and the next session is on
                    // its way over Wi-Fi; `local_ip` is cleared then, so
                    // that one is left alone.
                    let st = crate::stratum::state();
                    if st.connected && st.local_ip == ip {
                        crate::stratum::move_session(
                            "the pool session was on the Ethernet cable, which is gone",
                        );
                    }
                }
            }
            ip = now_ip;
        }
        // A link that never gets an address holds the default route away
        // from Wi-Fi (esp_netif counts an up netif, address or not):
        // stop and start the driver, which takes the netif down and
        // retries DHCP. The netif comes back on the next link-up event.
        if let (Some(t), None) = (link_since, &ip) {
            if t.elapsed() >= NO_ADDRESS {
                log::warn!(
                    "eth: link up for {} s without an address: restarting the driver so the default route is Wi-Fi meanwhile",
                    NO_ADDRESS.as_secs()
                );
                if let Err(e) = eth.stop() {
                    log::warn!("eth: stop failed: {e}");
                }
                std::thread::sleep(Duration::from_secs(2));
                if guard.as_ref().is_some_and(|g| {
                    g.active() && !static_ip::apply(eth.netif(), g.settings(), "ethernet")
                }) {
                    guard = None;
                }
                if let Err(e) = eth.start() {
                    log::error!("eth: start failed: {e}");
                }
                link_since = None;
                was_link = false;
                dhcp_failed = true;
            }
        }
        // The lock-out guard: no answer on the static address for a few
        // minutes of link and the cable goes back to DHCP for the boot.
        // The DHCP client is started explicitly between stop and start:
        // on the .41 the driver stop alone left the static address on the
        // netif (2026-11-25), and the start brought it back.
        if guard
            .as_mut()
            .is_some_and(|g| g.tick(eth.netif(), link && ip.is_some()) == Step::FallBack)
        {
            if let Err(e) = eth.stop() {
                log::warn!("eth: stop failed: {e}");
            }
            std::thread::sleep(Duration::from_secs(2));
            static_ip::back_to_dhcp(eth.netif());
            if let Err(e) = eth.start() {
                log::error!("eth: start failed: {e}");
            }
            link_since = None;
            was_link = false;
        }
        if link {
            down_since = None;
        } else if down_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(10) {
            waiting_since = None;
            dhcp_failed = false;
        }
        if ip.is_some() {
            dhcp_failed = false;
            waiting_since = None;
        } else if link {
            let since = *waiting_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= NO_ADDRESS {
                dhcp_failed = true;
            }
        }
        let (speed, duplex) = if link {
            link_params(&eth)
        } else {
            (None, None)
        };
        super::set_eth(|s| {
            s.eth_link = link;
            s.eth_connected = ip.is_some();
            s.eth_waiting = link && ip.is_none() && !dhcp_failed;
            s.eth_ip = ip.clone();
            s.eth_speed_mbit = speed;
            s.eth_full_duplex = duplex;
        });
        // A pool session over Wi-Fi moves to the cable once the cable has
        // held its address for a while: one reconnect, never a storm.
        if let (Some(a), Some(t)) = (&ip, ip_since) {
            if !moved && t.elapsed() >= SETTLE {
                let st = crate::stratum::state();
                // `local_ip` is IPv4 only (`Link::local_ip`). A session over
                // IPv6 runs on Wi-Fi in any case: only the station gets an
                // IPv6 address (net/wifi.rs), the cable none, so a cable
                // that comes or goes never concerns it.
                if st.connected && st.local_ip.as_deref().is_some_and(|l| l != a.as_str()) {
                    crate::stratum::move_session(&format!(
                        "the pool session runs over {} and the Ethernet cable has held {a} for {} s: moving it",
                        st.local_ip.as_deref().unwrap_or("?"),
                        SETTLE.as_secs()
                    ));
                }
                moved = true;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Negotiated speed (Mbit/s) and duplex, from the driver.
fn link_params<T>(eth: &EspEth<'_, T>) -> (Option<u16>, Option<bool>) {
    let h = eth.driver().handle();
    let mut speed: sys::eth_speed_t = 0;
    let mut duplex: sys::eth_duplex_t = 0;
    // SAFETY: plain out-parameters of the sizes the commands expect.
    let (rs, rd) = unsafe {
        (
            sys::esp_eth_ioctl(
                h,
                sys::esp_eth_io_cmd_t_ETH_CMD_G_SPEED,
                &mut speed as *mut _ as *mut core::ffi::c_void,
            ),
            sys::esp_eth_ioctl(
                h,
                sys::esp_eth_io_cmd_t_ETH_CMD_G_DUPLEX_MODE,
                &mut duplex as *mut _ as *mut core::ffi::c_void,
            ),
        )
    };
    (
        (rs == sys::ESP_OK).then(|| {
            if speed == sys::eth_speed_t_ETH_SPEED_100M {
                100
            } else {
                10
            }
        }),
        (rd == sys::ESP_OK).then(|| duplex == sys::eth_duplex_t_ETH_DUPLEX_FULL),
    )
}

fn link_text<T>(eth: &EspEth<'_, T>) -> String {
    match link_params(eth) {
        (Some(s), Some(d)) => format!(", {s} Mbit/s {} duplex", if d { "full" } else { "half" }),
        _ => String::new(),
    }
}

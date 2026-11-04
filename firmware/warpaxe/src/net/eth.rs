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

/// SPI clock, as NerdOS runs the HAT (long wires on a header).
const SPI_HZ: u32 = 2_000_000;
/// Without an interrupt line the driver asks the chip this often
/// (NerdOS: `poll_period_ms = 1`; the chip buffers 16 KB of frames).
const POLL: Duration = Duration::from_millis(1);
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
/// Ethernet holds an address this long before a pool session that runs
/// over Wi-Fi is moved onto it. A cable that comes and goes within the
/// time causes no reconnect of its own; the session that was on the
/// cable ends with the link anyway (lwIP aborts its socket with the
/// address), and the next one leaves over Wi-Fi.
const SETTLE: Duration = Duration::from_secs(30);

/// Look for the HAT on `pins` and run the link on its own thread. Called
/// once `esp_netif_init` has run and the board is known; the caller has
/// made sure the pins are free (`board::ethernet_pins`).
pub fn start(spi: SPI2, sysloop: EspSystemEventLoop, pins: EthPins, hostname: String) {
    std::thread::Builder::new()
        .stack_size(8192)
        .spawn(move || run(spi, sysloop, pins, hostname))
        .expect("eth thread");
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
    // The MAC stays the driver's: `esp_eth_driver_install` sets the ESP's
    // efuse Ethernet address (ESP_MAC_ETH), distinct from the Wi-Fi
    // station's and the USB link's.
    let driver = match EthDriver::new_spi_with_event_source(
        bus,
        event,
        Some(cs),
        rst,
        SpiEthChipset::W5500,
        Hertz(SPI_HZ),
        None,
        None,
        sysloop,
    ) {
        Ok(d) => d,
        Err(e) => {
            log::error!("eth: driver failed: {e}");
            super::set_eth(|s| s.eth_fitted = Some(false));
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
                return;
            }
        };
    if let Err(e) = eth.start() {
        log::error!("eth: start failed: {e}");
        return;
    }
    let mac = eth.netif().get_mac().ok().map(|m| {
        format!(
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    });
    log::info!("eth: started, MAC {}", mac.as_deref().unwrap_or("?"));
    super::set_eth(|s| s.eth_mac = mac);

    let mut was_link = false;
    let mut ip: Option<String> = None;
    // When the address came, for the move of a Wi-Fi pool session.
    let mut ip_since: Option<Instant> = None;
    let mut moved = false;
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
            } else {
                log::warn!("eth: link down");
            }
            was_link = link;
        }
        if now_ip != ip {
            match &now_ip {
                Some(a) => {
                    log::info!("eth: address {a}, the default route is the cable");
                    ip_since = Some(Instant::now());
                    moved = false;
                }
                None => {
                    log::warn!("eth: address gone, the default route is Wi-Fi (if up)");
                    ip_since = None;
                    // A pool session that was on the cable is dead with
                    // it; end it now rather than after the backoff.
                    if crate::stratum::state().local_ip == ip {
                        log::info!(
                            "eth: the pool session was on the cable, reconnecting over Wi-Fi"
                        );
                        crate::stratum::reconnect();
                    }
                }
            }
            ip = now_ip;
        }
        let (speed, duplex) = if link {
            link_params(&eth)
        } else {
            (None, None)
        };
        super::set_eth(|s| {
            s.eth_link = link;
            s.eth_connected = ip.is_some();
            s.eth_ip = ip.clone();
            s.eth_speed_mbit = speed;
            s.eth_full_duplex = duplex;
        });
        // A pool session over Wi-Fi moves to the cable once the cable has
        // held its address for a while: one reconnect, never a storm.
        if let (Some(a), Some(t)) = (&ip, ip_since) {
            if !moved && t.elapsed() >= SETTLE {
                let st = crate::stratum::state();
                if st.connected && st.local_ip.as_deref() != Some(a.as_str()) {
                    log::info!(
                        "eth: the pool session runs over {} after {} s on the cable: moving it to Ethernet",
                        st.local_ip.as_deref().unwrap_or("?"),
                        SETTLE.as_secs()
                    );
                    crate::stratum::reconnect();
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

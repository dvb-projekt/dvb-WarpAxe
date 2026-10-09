//! The chain's UART, as `warpaxe_chain::Uart`. UART1 on GPIO17 (TX) and
//! GPIO18 (RX): the pins every Nerd board routes to its carrier, and
//! where the USB-UART adapter for the simulator goes.

use esp_idf_svc::hal::gpio::{AnyIOPin, Gpio17, Gpio18};
use esp_idf_svc::hal::uart::{config::Config, UartDriver, UART1};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::{self, EspError};
use warpaxe_chain::Uart;

pub struct ChipUart {
    drv: UartDriver<'static>,
}

impl ChipUart {
    pub fn new(uart: UART1, tx: Gpio17, rx: Gpio18) -> Result<Self, EspError> {
        // Receive ring buffer well above the ESP-IDF default (2 × 128 B):
        // register replies come back to back, 11 bytes each, and the
        // mining thread drains every 2 ms. With the default, bursts
        // lost their tail (Gaia, 2026-10-01).
        let cfg = Config::new()
            .baudrate(Hertz(115_200))
            .rx_fifo_size(2048)
            .tx_fifo_size(512);
        let drv = UartDriver::new(
            uart,
            tx,
            rx,
            Option::<AnyIOPin>::None,
            Option::<AnyIOPin>::None,
            &cfg,
        )?;
        Ok(ChipUart { drv })
    }

    /// TX off the UART and driven low, for as long as the chips sit
    /// without their I/O rails (the Nerd carriers cut the LDO when the
    /// chips are switched off). An idle UART keeps TX at 3.3 V, and a
    /// chip whose I/O supply is gone gets that through the protection
    /// diode of its RX pin: the rail the LDO was meant to take away
    /// stays half up. A cold start has nothing driving the line; this
    /// is the nearest the firmware gets to it. Whatever is still in the
    /// TX FIFO goes out first.
    pub fn park(&mut self) -> Result<(), EspError> {
        self.drv.wait_tx_done(esp_idf_svc::hal::delay::BLOCK)?;
        unsafe {
            // Off the GPIO matrix (gpio_reset_pin leaves the pad as an
            // input with its pull-up: not that either).
            sys::gpio_reset_pin(TX_PIN);
            sys::gpio_set_pull_mode(TX_PIN, sys::gpio_pull_mode_t_GPIO_FLOATING);
            sys::gpio_set_level(TX_PIN, 0);
            sys::gpio_set_direction(TX_PIN, sys::gpio_mode_t_GPIO_MODE_OUTPUT);
        }
        Ok(())
    }

    /// TX back on the UART after [`park`](Self::park); the line idles
    /// high again. Done before the chain's reset pulse. `uart_set_pin`
    /// takes the IO MUX route for GPIO17 (U1TXD is the pad's own
    /// function, `uart_try_set_iomux_pin`): the pad's function select
    /// goes to the UART and `oen_sel` to 0, so the UART drives the
    /// output enable itself and the GPIO enable bit `park` left set
    /// does not matter — the same call, the same state as at boot.
    pub fn unpark(&mut self) -> Result<(), EspError> {
        esp_idf_svc::sys::esp!(unsafe {
            sys::uart_set_pin(
                UART_NUM,
                TX_PIN,
                sys::UART_PIN_NO_CHANGE,
                sys::UART_PIN_NO_CHANGE,
                sys::UART_PIN_NO_CHANGE,
            )
        })
    }
}

/// UART1 on GPIO17, as `new` takes them; the raw numbers for the pad
/// calls behind `park`.
const UART_NUM: sys::uart_port_t = 1;
const TX_PIN: i32 = 17;

impl Uart for ChipUart {
    type Error = EspError;

    fn write(&mut self, bytes: &[u8]) -> Result<(), EspError> {
        let mut off = 0;
        while off < bytes.len() {
            off += self.drv.write(&bytes[off..])?;
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize, EspError> {
        // Non-blocking: nothing pending is not an error.
        if self.drv.remaining_read()? == 0 {
            return Ok(0);
        }
        match self.drv.read(buf, 0) {
            Err(e) if e.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => Ok(0),
            r => r,
        }
    }

    fn set_baud(&mut self, baud: u32) -> Result<(), EspError> {
        // The frame that told the chips to switch is still in the TX
        // FIFO: let it leave at the old rate, give the chips a moment to
        // reconfigure, only then move. Found on the devkit with the
        // simulator behind a USB-UART adapter: without the wait the
        // frame's tail went out at the new rate and no chip followed.
        self.drv.wait_tx_done(esp_idf_svc::hal::delay::BLOCK)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.drv.change_baudrate(Hertz(baud)).map(|_| ())
    }
}

//! The chain's UART, as `warpaxe_chain::Uart`. UART1 on GPIO17 (TX) and
//! GPIO18 (RX): the pins every Nerd board routes to its carrier, and
//! where the USB-UART adapter for the simulator goes.

use esp_idf_svc::hal::gpio::{AnyIOPin, Gpio17, Gpio18};
use esp_idf_svc::hal::uart::{config::Config, UartDriver, UART1};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;
use warpaxe_chain::Uart;

pub struct ChipUart {
    drv: UartDriver<'static>,
}

impl ChipUart {
    pub fn new(uart: UART1, tx: Gpio17, rx: Gpio18) -> Result<Self, EspError> {
        let cfg = Config::new().baudrate(Hertz(115_200));
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
}

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

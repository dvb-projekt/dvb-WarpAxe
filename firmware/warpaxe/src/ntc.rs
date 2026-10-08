//! The thermistor divider of the NerdNOS on the ESP32-S3's ADC.
//!
//! One-shot reads through ESP-IDF's `adc_oneshot` driver with the
//! curve-fitting calibration the S3 has (the stock firmware used the
//! legacy `esp_adc_cal` on the same pins, 12 dB attenuation, 12 bits);
//! the core rail on GPIO3 reads the same way, without the thermistor
//! arithmetic.
//! The arithmetic from millivolts to degrees is
//! `warpaxe_board::ntc::Config`; this module only brings the voltage.

use std::sync::Mutex;

use esp_idf_svc::sys;
use warpaxe_board::ntc::Config;

/// Samples averaged per reading: the ADC's noise is a few mV, the
/// divider moves slowly.
const SAMPLES: usize = 8;

/// ESP-IDF hands out one one-shot handle per ADC unit; readers on
/// several pins (the thermistor, the core rail, the first-start probe)
/// share it. Created on first use, never deleted.
static UNIT: Mutex<Option<usize>> = Mutex::new(None);

fn unit() -> Option<sys::adc_oneshot_unit_handle_t> {
    let mut g = UNIT.lock().unwrap();
    if let Some(h) = *g {
        return Some(h as sys::adc_oneshot_unit_handle_t);
    }
    let init = sys::adc_oneshot_unit_init_cfg_t {
        unit_id: sys::adc_unit_t_ADC_UNIT_1,
        ..Default::default()
    };
    let mut h: sys::adc_oneshot_unit_handle_t = core::ptr::null_mut();
    let rc = unsafe { sys::adc_oneshot_new_unit(&init, &mut h) };
    if rc != sys::ESP_OK {
        log::warn!("ntc: adc unit failed ({rc})");
        return None;
    }
    *g = Some(h as usize);
    Some(h)
}

/// An ADC channel with a divider (or a rail) on it.
pub struct Reader {
    unit: sys::adc_oneshot_unit_handle_t,
    cali: sys::adc_cali_handle_t,
    channel: sys::adc_channel_t,
    config: Config,
}

// The handles are plain pointers into the driver; a reader is used from
// one thread at a time (the board loop), and the probe drops it.
unsafe impl Send for Reader {}

impl Reader {
    /// Open the divider's GPIO on ADC1. `None` when the pin is not an
    /// ADC1 pin or the driver refuses (logged).
    pub fn new(config: Config) -> Option<Reader> {
        Self::open(config.gpio, config)
    }

    /// A plain voltage on an ADC1 pin (the core rail sense): the same
    /// reader, the thermistor arithmetic unused.
    pub fn new_pin(gpio: u8) -> Option<Reader> {
        Self::open(gpio, Config::NERDNOS)
    }

    fn open(gpio: u8, config: Config) -> Option<Reader> {
        let mut unit_id: sys::adc_unit_t = 0;
        let mut channel: sys::adc_channel_t = 0;
        let rc =
            unsafe { sys::adc_oneshot_io_to_channel(i32::from(gpio), &mut unit_id, &mut channel) };
        if rc != sys::ESP_OK || unit_id != sys::adc_unit_t_ADC_UNIT_1 {
            log::warn!("ntc: GPIO{gpio} is not an ADC1 pin ({rc})");
            return None;
        }
        let unit = unit()?;
        let atten = sys::adc_atten_t_ADC_ATTEN_DB_12;
        let chan = sys::adc_oneshot_chan_cfg_t {
            atten,
            bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT,
        };
        let rc = unsafe { sys::adc_oneshot_config_channel(unit, channel, &chan) };
        if rc != sys::ESP_OK {
            log::warn!("ntc: adc channel on GPIO{gpio} failed ({rc})");
            return None;
        }
        let cali_cfg = sys::adc_cali_curve_fitting_config_t {
            unit_id,
            chan: channel,
            atten,
            bitwidth: sys::adc_bitwidth_t_ADC_BITWIDTH_DEFAULT,
        };
        let mut cali: sys::adc_cali_handle_t = core::ptr::null_mut();
        let rc = unsafe { sys::adc_cali_create_scheme_curve_fitting(&cali_cfg, &mut cali) };
        if rc != sys::ESP_OK {
            // Without eFuse calibration data the raw count stands in;
            // the 12 dB range is about 0–3.1 V over 4095 counts.
            log::warn!("ntc: no ADC calibration on GPIO{gpio} ({rc}), raw scale");
            cali = core::ptr::null_mut();
        }
        Some(Reader {
            unit,
            cali,
            channel,
            config,
        })
    }

    /// One raw conversion, converted to millivolts.
    fn sample_mv(&self) -> Option<u16> {
        let mut raw: i32 = 0;
        if unsafe { sys::adc_oneshot_read(self.unit, self.channel, &mut raw) } != sys::ESP_OK {
            return None;
        }
        if self.cali.is_null() {
            return Some((raw.clamp(0, 4095) * 3100 / 4095) as u16);
        }
        let mut mv: i32 = 0;
        (unsafe { sys::adc_cali_raw_to_voltage(self.cali, raw, &mut mv) } == sys::ESP_OK)
            .then(|| mv.clamp(0, 5000) as u16)
    }

    /// `SAMPLES` conversions, as taken.
    pub fn samples(&self) -> Vec<u16> {
        (0..SAMPLES).filter_map(|_| self.sample_mv()).collect()
    }

    /// The divider's voltage, averaged, mV.
    pub fn millivolts(&self) -> Option<u16> {
        let s = self.samples();
        if s.is_empty() {
            return None;
        }
        Some((s.iter().map(|&v| u32::from(v)).sum::<u32>() / s.len() as u32) as u16)
    }

    /// The thermistor's temperature, °C.
    pub fn celsius(&self) -> Option<f32> {
        self.config.celsius(self.millivolts()?)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.cali.is_null() {
            unsafe { sys::adc_cali_delete_scheme_curve_fitting(self.cali) };
        }
        // The unit stays: it is shared (see `UNIT`).
    }
}

/// Read the divider once, for the first-start probe: the samples and,
/// if they look like a thermistor rather than a floating pin, the
/// temperature. Read-only; the pin is released afterwards.
pub fn probe(config: Config) -> (Vec<u16>, Option<f32>) {
    let Some(r) = Reader::new(config) else {
        return (Vec::new(), None);
    };
    let s = r.samples();
    let t = config.plausible(&s);
    (s, t)
}

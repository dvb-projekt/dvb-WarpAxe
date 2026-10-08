//! A thermistor on an ADC pin: the NerdNOS's board temperature.
//!
//! The NerdNOS (benjamin-wilson/NerdNOS, `bm1397.kicad_sch`) has a 10 kΩ
//! NTC (TH1, NTCG103JF103FT1, B = 3380 K) from the sense net to ground
//! and a 10 kΩ pull-up (R8) to 3.3 V; the stock firmware reads it on
//! ADC1 channel 1, GPIO2 (WantClue `adc.cpp`: `R_T = R0 · V / (3.3 − V)`,
//! `T = B / (ln(R_T / R0) + B / 298.15)`). It sits next to the chip, not
//! on its die: NerdNOS stops at 80 °C there and resumes 10 °C lower.
//!
//! Only the arithmetic lives here; the firmware brings the ADC.

/// What the divider is made of.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// ESP GPIO the divider's midpoint is on (an ADC1 pin).
    pub gpio: u8,
    /// The thermistor's B constant, K.
    pub beta: f32,
    /// Its resistance at 25 °C, Ω.
    pub r0_ohm: f32,
    /// The pull-up from the midpoint to the reference, Ω.
    pub r_pullup_ohm: f32,
    /// The divider's supply, mV.
    pub vref_mv: u16,
}

impl Config {
    /// The NerdNOS divider as above.
    pub const NERDNOS: Config = Config {
        gpio: 2,
        beta: 3380.0,
        r0_ohm: 10_000.0,
        r_pullup_ohm: 10_000.0,
        vref_mv: 3300,
    };

    /// Temperature for a midpoint voltage, °C; `None` when the voltage
    /// is at a rail (the divider is open or shorted: no thermistor to
    /// speak of). The stock firmware returns −273.15 for a zero reading.
    pub fn celsius(&self, mv: u16) -> Option<f32> {
        let v = f32::from(mv);
        let vref = f32::from(self.vref_mv);
        if v < 1.0 || v >= vref - 1.0 {
            return None;
        }
        let r = self.r_pullup_ohm * v / (vref - v);
        let kelvin = self.beta / (libm::logf(r / self.r0_ohm) + self.beta / 298.15);
        Some(kelvin - 273.15)
    }

    /// The midpoint voltage a temperature gives, mV: the inverse of
    /// [`Config::celsius`], for tests and for the detection window.
    pub fn millivolts(&self, celsius: f32) -> u16 {
        let kelvin = celsius + 273.15;
        let r = self.r0_ohm * libm::expf(self.beta * (1.0 / kelvin - 1.0 / 298.15));
        let v = f32::from(self.vref_mv) * r / (r + self.r_pullup_ohm);
        v as u16
    }

    /// Does a set of readings look like this divider with a thermistor
    /// at room temperature or a warm board, rather than a floating pin?
    /// Every sample between −10 and 90 °C and the spread below 100 mV:
    /// a floating ESP32-S3 input drifts and reads near a rail, a divider
    /// holds still in the middle. Returns the mean temperature.
    pub fn plausible(&self, samples_mv: &[u16]) -> Option<f32> {
        let (lo, hi) = (self.millivolts(90.0), self.millivolts(-10.0));
        let min = *samples_mv.iter().min()?;
        let max = *samples_mv.iter().max()?;
        if min < lo || max > hi || max - min > 100 {
            return None;
        }
        let sum: u32 = samples_mv.iter().map(|&s| u32::from(s)).sum();
        self.celsius((sum / samples_mv.len() as u32) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: Config = Config::NERDNOS;

    #[test]
    fn room_temperature_is_half_the_rail() {
        // R = R0 at 25 °C: the midpoint is half of 3.3 V.
        assert_eq!(N.millivolts(25.0), 1650);
        let t = N.celsius(1650).unwrap();
        assert!((t - 25.0).abs() < 0.05, "{t}");
    }

    /// The values the stock firmware's formula gives (computed in f64):
    /// 32 °C is what the .46 reported on 2026-10-08, 80 °C its stop.
    #[test]
    fn matches_the_beta_formula() {
        for (c, mv) in [(0.0, 2437), (32.0, 1437), (50.0, 970), (80.0, 482)] {
            let got = N.millivolts(c);
            assert!(
                (i32::from(got) - mv).abs() <= 2,
                "{c} °C: {got} mV, want {mv}"
            );
            let back = N.celsius(mv as u16).unwrap();
            assert!((back - c).abs() < 0.2, "{c} °C from {mv} mV: {back}");
        }
    }

    #[test]
    fn rails_are_no_reading() {
        assert_eq!(N.celsius(0), None);
        assert_eq!(N.celsius(3300), None);
        assert_eq!(N.celsius(3299), None);
        assert!(N.celsius(1).is_some());
    }

    #[test]
    fn plausibility_wants_a_steady_midpoint() {
        let t = N.plausible(&[1450, 1460, 1455, 1458]).unwrap();
        assert!((t - 32.0).abs() < 1.0, "{t}");
        assert_eq!(N.plausible(&[0, 0, 0, 0]), None, "floating low");
        assert_eq!(N.plausible(&[3100, 3100, 3100]), None, "floating high");
        assert_eq!(N.plausible(&[1450, 1600, 1450, 1700]), None, "noise");
        assert_eq!(N.plausible(&[]), None);
        // 90 °C and −10 °C are the window's edges.
        assert!(N.plausible(&[N.millivolts(89.0)]).is_some());
        assert!(N.plausible(&[N.millivolts(91.0) - 5]).is_none());
        assert!(N.plausible(&[N.millivolts(-9.0)]).is_some());
    }
}

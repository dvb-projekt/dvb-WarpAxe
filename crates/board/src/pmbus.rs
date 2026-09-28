//! PMBus number formats and command codes (the subset the TPS546 and
//! TPS53647/TPS53667 need).
//!
//! LINEAR11: 5-bit signed exponent, 11-bit signed mantissa, value =
//! mantissa × 2^exponent. ULINEAR16: 16-bit unsigned mantissa with the
//! exponent taken from VOUT_MODE. Encoding picks the finest exponent
//! whose mantissa still fits, like ESP-Miner's `float_2_slinear11`.

/// PMBus command code.
pub const OPERATION: u8 = 0x01;
/// PMBus command code.
pub const ON_OFF_CONFIG: u8 = 0x02;
/// PMBus command code.
pub const CLEAR_FAULTS: u8 = 0x03;
/// PMBus command code.
pub const PHASE: u8 = 0x04;
/// PMBus command code.
pub const RESTORE_DEFAULT_ALL: u8 = 0x12;
/// PMBus command code.
pub const VOUT_MODE: u8 = 0x20;
/// PMBus command code.
pub const VOUT_COMMAND: u8 = 0x21;
/// PMBus command code.
pub const VOUT_MAX: u8 = 0x24;
/// PMBus command code.
pub const VOUT_MARGIN_HIGH: u8 = 0x25;
/// PMBus command code.
pub const VOUT_MARGIN_LOW: u8 = 0x26;
/// PMBus command code.
pub const VOUT_SCALE_LOOP: u8 = 0x29;
/// PMBus command code.
pub const VOUT_MIN: u8 = 0x2B;
/// PMBus command code.
pub const FREQUENCY_SWITCH: u8 = 0x33;
/// PMBus command code.
pub const VIN_ON: u8 = 0x35;
/// PMBus command code.
pub const VIN_OFF: u8 = 0x36;
/// PMBus command code.
pub const VOUT_OV_FAULT_LIMIT: u8 = 0x40;
/// PMBus command code.
pub const VOUT_OV_WARN_LIMIT: u8 = 0x42;
/// PMBus command code.
pub const VOUT_UV_WARN_LIMIT: u8 = 0x43;
/// PMBus command code.
pub const VOUT_UV_FAULT_LIMIT: u8 = 0x44;
/// PMBus command code.
pub const IOUT_OC_FAULT_LIMIT: u8 = 0x46;
/// PMBus command code.
pub const IOUT_OC_FAULT_RESPONSE: u8 = 0x47;
/// PMBus command code.
pub const IOUT_OC_WARN_LIMIT: u8 = 0x4A;
/// PMBus command code.
pub const OT_FAULT_LIMIT: u8 = 0x4F;
/// PMBus command code.
pub const OT_FAULT_RESPONSE: u8 = 0x50;
/// PMBus command code.
pub const OT_WARN_LIMIT: u8 = 0x51;
/// PMBus command code.
pub const VIN_OV_FAULT_LIMIT: u8 = 0x55;
/// PMBus command code.
pub const VIN_OV_FAULT_RESPONSE: u8 = 0x56;
/// PMBus command code.
pub const VIN_UV_WARN_LIMIT: u8 = 0x58;
/// PMBus command code.
pub const IIN_OC_FAULT_LIMIT: u8 = 0x5B;
/// PMBus command code.
pub const IIN_OC_WARN_LIMIT: u8 = 0x5D;
/// PMBus command code.
pub const TON_DELAY: u8 = 0x60;
/// PMBus command code.
pub const TON_RISE: u8 = 0x61;
/// PMBus command code.
pub const TON_MAX_FAULT_LIMIT: u8 = 0x62;
/// PMBus command code.
pub const TON_MAX_FAULT_RESPONSE: u8 = 0x63;
/// PMBus command code.
pub const TOFF_DELAY: u8 = 0x64;
/// PMBus command code.
pub const TOFF_FALL: u8 = 0x65;
/// PMBus command code.
pub const STATUS_BYTE: u8 = 0x78;
/// PMBus command code.
pub const STATUS_WORD: u8 = 0x79;
/// PMBus command code.
pub const STATUS_VOUT: u8 = 0x7A;
/// PMBus command code.
pub const STATUS_IOUT: u8 = 0x7B;
/// PMBus command code.
pub const STATUS_INPUT: u8 = 0x7C;
/// PMBus command code.
pub const STATUS_TEMPERATURE: u8 = 0x7D;
/// PMBus command code.
pub const STATUS_CML: u8 = 0x7E;
/// PMBus command code.
pub const STATUS_MFR_SPECIFIC: u8 = 0x80;
/// PMBus command code.
pub const READ_VIN: u8 = 0x88;
/// PMBus command code.
pub const READ_IIN: u8 = 0x89;
/// PMBus command code.
pub const READ_VOUT: u8 = 0x8B;
/// PMBus command code.
pub const READ_IOUT: u8 = 0x8C;
/// PMBus command code.
pub const READ_TEMPERATURE_1: u8 = 0x8D;
/// PMBus command code.
pub const READ_POUT: u8 = 0x96;
/// PMBus command code.
pub const READ_PIN: u8 = 0x97;
/// PMBus command code.
pub const IC_DEVICE_ID: u8 = 0xAD;
/// PMBus command code.
pub const COMPENSATION_CONFIG: u8 = 0xB1;
/// PMBus command code.
pub const SYNC_CONFIG: u8 = 0xE4;
/// PMBus command code.
pub const STACK_CONFIG: u8 = 0xEC;
/// PMBus command code.
pub const PIN_DETECT_OVERRIDE: u8 = 0xEE;

/// Sign-extend the low `bits` of `v`.
fn sext(v: u16, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((v as i32) << shift) >> shift
}

fn pow2(exp: i32) -> f32 {
    if exp >= 0 {
        (1u64 << exp) as f32
    } else {
        1.0 / (1u64 << (-exp)) as f32
    }
}

/// LINEAR11 word to a value.
pub fn linear11_to_f32(word: u16) -> f32 {
    let exponent = sext(word >> 11, 5);
    let mantissa = sext(word & 0x7FF, 11);
    mantissa as f32 * pow2(exponent)
}

/// LINEAR11 word to an integer (temperatures), truncating.
pub fn linear11_to_i32(word: u16) -> i32 {
    linear11_to_f32(word) as i32
}

/// A value to LINEAR11 with the finest exponent that keeps the mantissa
/// within ±1023; 0 encodes as 0.
pub fn f32_to_linear11(value: f32) -> u16 {
    if value == 0.0 {
        return 0;
    }
    let magnitude = if value < 0.0 { -value } else { value };
    // Scale up while the mantissa still fits (negative exponents)...
    let mut shift: i32 = 0;
    while shift < 15 && magnitude * pow2(shift + 1) < 1024.0 {
        shift += 1;
    }
    // ...or down when the plain value does not fit (positive exponents).
    let mut exponent = -shift;
    while magnitude * pow2(-exponent) >= 1024.0 && exponent < 15 {
        exponent += 1;
    }
    let mantissa = (value * pow2(-exponent)) as i32;
    (((exponent as u16) & 0x1F) << 11) | ((mantissa as u16) & 0x7FF)
}

/// An integer to LINEAR11 (exponent 0 when it fits).
pub fn i32_to_linear11(value: i32) -> u16 {
    if (-1024..=1023).contains(&value) {
        (value as u16) & 0x7FF
    } else {
        f32_to_linear11(value as f32)
    }
}

/// The exponent VOUT_MODE carries in its low five bits (linear mode).
pub fn vout_mode_exponent(vout_mode: u8) -> i32 {
    sext(u16::from(vout_mode & 0x1F), 5)
}

/// ULINEAR16 word to volts with the VOUT_MODE exponent.
pub fn ulinear16_to_f32(word: u16, exponent: i32) -> f32 {
    word as f32 * pow2(exponent)
}

/// Volts to ULINEAR16 with the VOUT_MODE exponent, saturating.
pub fn f32_to_ulinear16(value: f32, exponent: i32) -> u16 {
    let scaled = value * pow2(-exponent);
    if scaled <= 0.0 {
        0
    } else if scaled >= 65535.0 {
        u16::MAX
    } else {
        scaled as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear11_matches_the_reference_encoding() {
        // 4.8 V: mantissa 614 at 2^-7, as ESP-Miner's loop produces it.
        assert_eq!(f32_to_linear11(4.8), 0xCA66);
        assert!((linear11_to_f32(0xCA66) - 4.8).abs() < 0.01);
        assert_eq!(i32_to_linear11(1), 0x0001);
        assert_eq!(i32_to_linear11(105), 105);
        assert_eq!(i32_to_linear11(-1), 0x07FF);
        assert_eq!(linear11_to_i32(0x07FF), -1);
        assert_eq!(f32_to_linear11(0.0), 0);
    }

    #[test]
    fn linear11_round_trips_the_values_the_regulator_uses() {
        for v in [
            0.125f32, 0.25, 1.0, 3.0, 4.5, 6.5, 11.5, 14.8, 25.0, 30.0, 650.0, 4000.0,
        ] {
            let back = linear11_to_f32(f32_to_linear11(v));
            assert!((back - v).abs() <= v * 0.002, "{v} -> {back}");
        }
        assert_eq!(i32_to_linear11(2000), f32_to_linear11(2000.0));
        assert_eq!(linear11_to_i32(i32_to_linear11(2000)), 2000);
    }

    #[test]
    fn ulinear16_uses_the_vout_mode_exponent() {
        // TPS546D24A reports VOUT_MODE 0x17: exponent -9, 1 LSB = 1/512 V.
        assert_eq!(vout_mode_exponent(0x17), -9);
        assert_eq!(f32_to_ulinear16(1.2, -9), 614);
        assert!((ulinear16_to_f32(614, -9) - 1.199).abs() < 0.002);
        assert_eq!(f32_to_ulinear16(0.0, -9), 0);
        assert_eq!(f32_to_ulinear16(1000.0, -9), u16::MAX);
    }
}

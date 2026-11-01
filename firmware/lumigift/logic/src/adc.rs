//! ADC conversions: VDDA from VREFINT, pin voltage, internal temperature.
//!
//! Factory calibration values (STM32C011 datasheet, "Reference voltage" and
//! "Temperature sensor characteristics"): `VREFINT_CAL` and `TS_CAL1` are raw
//! 12-bit readings taken at 30 °C with VDDA = 3.0 V.

/// VDDA used for the factory calibration.
pub const CAL_VDDA_MV: u32 = 3000;
/// Typical VREFINT (used if the calibration word looks unprogrammed).
pub const VREFINT_TYP_MV: u32 = 1212;
/// Typical sensor voltage at 30 °C and slope (datasheet typ. values).
pub const TS_V30_TYP_MV: u32 = 760;
pub const TS_SLOPE_UV_PER_C: i32 = 2530;
/// VBAT below this sets `LOW_BATTERY` (one AAA cell straight to the pin).
pub const LOW_BATTERY_MV: u16 = 1000;
/// ...and it clears again above this.
pub const LOW_BATTERY_CLEAR_MV: u16 = 1050;

const FULL_SCALE: u32 = 4095;

/// VDDA in mV from a VREFINT reading.
pub fn vdda_mv(vrefint_raw: u16, vrefint_cal: u16) -> u32 {
    let cal = if (1300..=1900).contains(&vrefint_cal) {
        vrefint_cal as u32
    } else {
        VREFINT_TYP_MV * FULL_SCALE / CAL_VDDA_MV
    };
    if vrefint_raw == 0 {
        return 3300;
    }
    CAL_VDDA_MV * cal / vrefint_raw as u32
}

/// Pin voltage in mV for a 12-bit reading at the given VDDA.
pub fn pin_mv(raw: u16, vdda_mv: u32) -> u16 {
    (raw as u32 * vdda_mv / FULL_SCALE) as u16
}

/// Die temperature in °C from the sensor reading.
pub fn temp_c(ts_raw: u16, vdda_mv: u32, ts_cal1: u16) -> i8 {
    let v30_uv = if (700..=1300).contains(&ts_cal1) {
        ts_cal1 as i64 * CAL_VDDA_MV as i64 * 1000 / FULL_SCALE as i64
    } else {
        TS_V30_TYP_MV as i64 * 1000
    };
    let v_uv = ts_raw as i64 * vdda_mv as i64 * 1000 / FULL_SCALE as i64;
    (30 + (v_uv - v30_uv) / TS_SLOPE_UV_PER_C as i64).clamp(-128, 127) as i8
}

/// Low-battery flag with hysteresis.
pub fn low_battery(prev: bool, vbat_mv: u16) -> bool {
    if prev { vbat_mv < LOW_BATTERY_CLEAR_MV } else { vbat_mv < LOW_BATTERY_MV }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        // VREFINT 1.212 V calibrated at 3.0 V -> 1654; at VDDA 3.3 V reads 1504.
        let v = vdda_mv(1504, 1654);
        assert!((3290..=3310).contains(&v), "{v}");
        // 1.2 V cell at 3.3 V.
        let mv = pin_mv(1489, 3300);
        assert!((1195..=1205).contains(&mv), "{mv}");
        // Unprogrammed calibration falls back to typical.
        assert!((3280..=3320).contains(&vdda_mv(1504, 0xFFFF)));
        assert!(low_battery(false, 990));
        assert!(low_battery(true, 1020));
        assert!(!low_battery(true, 1060));
        assert!(!low_battery(false, 1020));
    }

    #[test]
    fn temperature() {
        // At 30 °C and VDDA = 3.0 V the reading equals TS_CAL1.
        assert_eq!(temp_c(1037, 3000, 1037), 30);
        // +10 °C = +25.3 mV = ~31 LSB at 3.3 V... compute at 3.0 V: 34.5 LSB.
        let t = temp_c(1037 + 35, 3000, 1037);
        assert!((39..=41).contains(&t), "{t}");
        // Same voltage measured at VDDA 3.3 V gives the same temperature.
        let raw33 = (1037u32 * 3000 / 3300) as u16;
        assert!((29..=31).contains(&temp_c(raw33, 3300, 1037)));
    }
}

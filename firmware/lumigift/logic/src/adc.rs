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

/// LED budget in mA at 5 V that a cell at `vbat_mv` (measured under load)
/// can feed without browning out: the current an AAA cell can deliver (~700
/// mA fresh, ~60 mA nearly flat) times its voltage, through the ~80 %
/// efficient boost converter. The configured budget (250 mA at 5 V by
/// default) would need ~1 A from the cell, which browned out a gift on a
/// 0.8 V cell into recovery. `0` (not measured yet) means no limit.
pub fn battery_led_budget_ma(vbat_mv: u16) -> u16 {
    // (cell mV, cell mA it can sustain)
    const CURVE: [(u32, u32); 6] = [(800, 60), (900, 150), (1000, 250), (1100, 350), (1200, 450), (1350, 700)];
    if vbat_mv == 0 {
        return u16::MAX;
    }
    let v = vbat_mv as u32;
    let i = match CURVE.iter().position(|&(cv, _)| v < cv) {
        Some(0) => CURVE[0].1,
        Some(k) => {
            let ((v0, i0), (v1, i1)) = (CURVE[k - 1], CURVE[k]);
            i0 + (i1 - i0) * (v - v0) / (v1 - v0)
        }
        None => CURVE[CURVE.len() - 1].1,
    };
    (v * i * 4 / 5 / 5000) as u16
}

/// Track the battery budget: down at once, up by at most `step` per call (the
/// voltage is measured under the very load it limits; following it upwards
/// at once would make the LEDs pump).
pub fn follow_budget(cap: u16, target: u16, step: u16) -> u16 {
    if target <= cap { target } else { cap.saturating_add(step).min(target) }
}

/// Battery voltage filter: an exponential average in mV x 8 (alpha 1/8,
/// ~0.8 s at 10 readings/s). Single readings swing by ±150 mV with the boost
/// converter's pulsed load, which made the LED cap (and the LEDs) pump. The
/// first reading (or the first after a 0 = no reading) is taken as is.
pub fn filter_vbat(prev_x8: u32, mv: u16) -> u32 {
    if mv == 0 {
        0
    } else if prev_x8 == 0 {
        mv as u32 * 8
    } else {
        prev_x8 - prev_x8 / 8 + mv as u32
    }
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
    fn vbat_filter_smooths() {
        let mut f = filter_vbat(0, 1500);
        assert_eq!(f / 8, 1500);
        // Alternating ±150 mV readings settle within a few mV of the mean.
        for i in 0..100 {
            f = filter_vbat(f, if i % 2 == 0 { 1350 } else { 1650 });
        }
        assert!((f / 8).abs_diff(1500) <= 20, "{}", f / 8);
        // A real drop comes through within ~2 s (20 readings).
        for _ in 0..20 {
            f = filter_vbat(f, 1000);
        }
        assert!(f / 8 < 1050, "{}", f / 8);
        assert_eq!(filter_vbat(f, 0), 0);
    }

    #[test]
    fn battery_budget() {
        assert_eq!(battery_led_budget_ma(0), u16::MAX);
        // Fresh alkaline / bench supply: ~150 mA at 5 V.
        let fresh = battery_led_budget_ma(1450);
        assert!((140..=170).contains(&fresh), "{fresh}");
        let half = battery_led_budget_ma(1150);
        assert!((60..=80).contains(&half), "{half}");
        // Nearly flat: barely glowing instead of browning out.
        assert!(battery_led_budget_ma(800) < 10);
        assert!(battery_led_budget_ma(700) < 10);
        // Monotonic.
        let mut prev = 0;
        for mv in (700..1600).step_by(10) {
            let b = battery_led_budget_ma(mv);
            assert!(b >= prev, "{mv}: {b} < {prev}");
            prev = b;
        }
        // Down at once, up slowly.
        assert_eq!(follow_budget(150, 40, 10), 40);
        assert_eq!(follow_budget(40, 150, 10), 50);
        assert_eq!(follow_budget(145, 150, 10), 150);
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

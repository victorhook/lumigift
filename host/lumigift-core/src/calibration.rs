//! Two-point calibration of a gift's battery measurement (bench supply).
//!
//! The gift corrects every reading as `raw + raw * gain / 32768 + offset`
//! (`lumigift_proto::layout::calibrate_vbat_mv`, keys
//! [`config_key::VBAT_GAIN`](crate::config_key::VBAT_GAIN) /
//! [`VBAT_OFFSET_MV`](crate::config_key::VBAT_OFFSET_MV)). Measure the raw
//! reading (calibration reset to 0) at two known supply voltages and this
//! gives the values to store.

/// Points closer than this give a poor gain estimate.
pub const MIN_SPAN_MV: f64 = 200.0;
/// Larger corrections mean a wiring or entry mistake, not ADC error.
pub const MAX_GAIN_ERROR: f64 = 0.2;
pub const MAX_OFFSET_MV: f64 = 254.0;

/// `(gain, offset_mv)` to store, from two `(raw_mv, true_mv)` points.
pub fn vbat_calibration(p1: (f64, f64), p2: (f64, f64)) -> Result<(i16, i16), String> {
    let ((r1, t1), (r2, t2)) = (p1, p2);
    if (t2 - t1).abs() < MIN_SPAN_MV {
        return Err(format!(
            "the two supply voltages must be at least {:.1} V apart",
            MIN_SPAN_MV / 1000.0
        ));
    }
    if (r2 - r1).abs() < 1.0 || (r2 - r1).signum() != (t2 - t1).signum() {
        return Err(format!(
            "the gift's reading did not follow the supply ({r1:.0} mV -> {r2:.0} mV): is it powered from the supply?"
        ));
    }
    let a = (t2 - t1) / (r2 - r1);
    let b = t1 - a * r1;
    if (a - 1.0).abs() > MAX_GAIN_ERROR {
        return Err(format!(
            "gain error {:+.1} % is implausible (max ±{:.0} %): check the voltages",
            (a - 1.0) * 100.0,
            MAX_GAIN_ERROR * 100.0
        ));
    }
    if b.abs() > MAX_OFFSET_MV {
        return Err(format!("offset {b:+.0} mV is out of range (max ±{MAX_OFFSET_MV:.0} mV)"));
    }
    Ok((((a - 1.0) * 32768.0).round() as i16, b.round() as i16))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumigift_proto::layout::calibrate_vbat_mv;

    #[test]
    fn recovers_gain_and_offset() {
        // A gift reading 3 % high plus 25 mV.
        let raw = |t: f64| t * 1.03 + 25.0;
        let (g, o) = vbat_calibration((raw(1000.0), 1000.0), (raw(1500.0), 1500.0)).unwrap();
        for t in [900.0, 1000.0, 1200.0, 1500.0] {
            let c = calibrate_vbat_mv(raw(t).round() as u16, g, (o / 2) as i8);
            assert!((c as f64 - t).abs() <= 3.0, "{t} mV -> {c} mV (gain {g}, offset {o})");
        }
    }

    #[test]
    fn rejects_bad_points() {
        assert!(vbat_calibration((1000.0, 1000.0), (1100.0, 1100.0)).is_err()); // too close
        assert!(vbat_calibration((1000.0, 1000.0), (1000.0, 1500.0)).is_err()); // reading stuck
        assert!(vbat_calibration((1000.0, 1000.0), (2000.0, 1500.0)).is_err()); // gain -50 %
        assert!(vbat_calibration((700.0, 1000.0), (1200.0, 1500.0)).is_err()); // offset +300 mV
        assert_eq!(vbat_calibration((1000.0, 1000.0), (1500.0, 1500.0)), Ok((0, 0)));
    }
}

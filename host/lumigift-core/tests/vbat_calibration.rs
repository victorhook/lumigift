//! Battery measurement calibration through the whole stack: a simulated gift
//! with ADC error, two supply voltages, the stored correction.

use std::time::Duration;

use lumigift_core::calibration::vbat_calibration;
use lumigift_core::sim::SimConfig;
use lumigift_core::*;

#[test]
fn two_point_calibration_corrects_the_gift() {
    let mut sc = SimConfig::new(1);
    sc.loss_pct = 0.0;
    sc.gifts[0].vbat_error = (1.03, 25.0); // reads 3 % high + 25 mV
    let cfg = LinkConfig { metadata: MetadataLocation::Disabled, wake_on_connect: false, ..Default::default() };
    let (link, sim) = Link::simulated(cfg, sc);
    let snap = link
        .wait_for(Duration::from_secs(10), |s| s.gifts.iter().any(|g| g.online))
        .expect("gift online");
    let uid = snap.gifts[0].uid;
    let read = |mv: u16| {
        sim.set_vbat(0, mv);
        std::thread::sleep(Duration::from_millis(600));
        link.snapshot().gifts[0].vbat_mv as f64
    };
    let (r1, r2) = (read(1000), read(1500));
    assert!((r1 - 1055.0).abs() < 3.0, "raw at 1.0 V: {r1}");
    let (gain, offset) = vbat_calibration((r1, 1000.0), (r2, 1500.0)).expect("plausible");
    for (k, v) in [(config_key::VBAT_GAIN, gain), (config_key::VBAT_OFFSET_MV, offset)] {
        let r = link.set_config(Target::Gifts(vec![uid]), k, v as u16).wait().expect("sent");
        assert!(r.all_ok(), "{r}");
    }
    for mv in [900u16, 1200, 1500] {
        let got = read(mv);
        assert!((got - mv as f64).abs() <= 4.0, "{mv} mV reads {got} after calibration");
    }
    link.shutdown();
}

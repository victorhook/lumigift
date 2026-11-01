//! VBAT (PA3 = ADC_IN3), VREFINT (ADC_IN10) and the temperature sensor
//! (ADC_IN9), single conversions on demand (RM0490 §15).

use lumigift_board::pac::adc::regs::Chselr;
use lumigift_board::pac::adc::vals::{Ckmode, SampleTime};
use lumigift_board::pac::{ADC1, ADC1_COMMON, RCC};
use lumigift_board::time::delay_us;
use lumigift_logic::adc::{pin_mv, temp_c, vdda_mv};

const CH_VBAT: u32 = 3;
const CH_TEMP: u32 = 9;
const CH_VREFINT: u32 = 10;
/// Factory calibration (datasheet "Temperature sensor / internal reference
/// calibration values"), raw 12-bit at 30 °C, VDDA = 3.0 V.
const TS_CAL1: *const u16 = 0x1FFF_7568 as *const u16;
const VREFINT_CAL: *const u16 = 0x1FFF_756A as *const u16;

pub struct Adc;

pub struct Reading {
    pub vbat_mv: u16,
    pub temp_c: i8,
}

impl Adc {
    pub fn new() -> Self {
        RCC.apbenr2().modify(|w| w.set_adc1en(true));
        // PCLK/4 = 12 MHz: 160.5-cycle sampling = 13 µs, above the minimum
        // sampling time of the internal channels.
        ADC1.cfgr2().write(|w| w.set_ckmode(Ckmode::PCLK_DIV_4));
        ADC1.cr().modify(|w| w.set_advregen(true));
        delay_us(30); // tADCVREG_STUP
        ADC1.cr().modify(|w| w.set_adcal(true));
        while ADC1.cr().read().adcal() {}
        delay_us(5);
        ADC1.smpr().write(|w| w.set_smp1(SampleTime::from_bits(0b111))); // 160.5 cycles
        ADC1_COMMON.ccr().modify(|w| {
            w.set_vrefen(true);
            w.set_tsen(true);
        });
        ADC1.isr().write(|w| w.set_adrdy(true));
        ADC1.cr().modify(|w| w.set_aden(true));
        while !ADC1.isr().read().adrdy() {}
        delay_us(20); // sensor start-up
        Self
    }

    fn convert(&mut self, ch: u32) -> u16 {
        ADC1.isr().write(|w| w.set_ccrdy(true));
        ADC1.chselr().write_value(Chselr(1 << ch));
        while !ADC1.isr().read().ccrdy() {}
        ADC1.cr().modify(|w| w.set_adstart(true));
        while !ADC1.isr().read().eoc() {}
        ADC1.dr().read().data()
    }

    pub fn read(&mut self) -> Reading {
        let vref = self.convert(CH_VREFINT);
        // SAFETY: factory calibration words in system memory.
        let (vcal, tcal) = unsafe { (core::ptr::read_volatile(VREFINT_CAL), core::ptr::read_volatile(TS_CAL1)) };
        let vdda = vdda_mv(vref, vcal);
        let mut sum = 0u32;
        for _ in 0..4 {
            sum += self.convert(CH_VBAT) as u32;
        }
        let vbat_mv = pin_mv((sum / 4) as u16, vdda);
        let temp_c = temp_c(self.convert(CH_TEMP), vdda, tcal);
        Reading { vbat_mv, temp_c }
    }
}

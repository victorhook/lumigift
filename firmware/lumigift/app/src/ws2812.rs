//! WS2812B driver: TIM1_CH3 PWM on PA2, one DMA transfer per frame.
//!
//! TIM1 runs at 48 MHz with ARR = 59, so one PWM period is 1.25 µs
//! (800 kHz). Each bit is one period with CCR3 = 17 ticks (0.354 µs high,
//! a "0") or 38 ticks (0.792 µs high, a "1"). DMA1 channel 1, requested by
//! the TIM1 update event through DMAMUX (request 25), writes the next duty
//! value into the CCR3 preload register on every update; with OC3PE set it
//! becomes active at the following update, so DMA latency never distorts a
//! bit. The buffer is framed by a 0 duty at both ends: the trailing 0 stays
//! in CCR3 after the transfer, holding the line low (reset/latch) until the
//! next frame 10 ms later (WS2812B needs ≥ 50 µs, newer parts ≥ 280 µs).
//!
//! Bits are stored as bytes (MSIZE 8, PSIZE 16: the DMA zero-extends), so
//! a frame costs 98 bytes of RAM and the CPU only touches the buffer while
//! the channel is idle. CPU stalls during flash writes do not matter: the
//! DMA reads SRAM and writes the timer without the CPU.

use core::cell::UnsafeCell;

use lumigift_board::gpio;
use lumigift_board::pac::bdma::vals::{Dir, Pl, Size};
use lumigift_board::pac::gpio::vals::Pupdr;
use lumigift_board::pac::timer::regs::{ArrCore, Ccr1ch};
use lumigift_board::pac::timer::vals::OcmGp;
use lumigift_board::pac::{DMA1, DMAMUX1, GPIOA, RCC, TIM1};
use lumigift_proto::{LEDS_PER_GIFT, Rgb};

const ARR: u16 = 59; // 48 MHz / 60 = 800 kHz
const T0H: u8 = 17; // 0.354 µs
const T1H: u8 = 38; // 0.792 µs
const LEN: usize = 1 + LEDS_PER_GIFT * 24 + 1;
/// DMAMUX request line of TIM1_UP (RM0490 Table "DMAMUX requests").
const DMAREQ_TIM1_UP: u8 = 25;
/// DMA1 channel 1 = DMAMUX channel 0.
const CH: usize = 0;
const TIM1_AF_PA2: u8 = 5;

struct DmaBuf(UnsafeCell<[u8; LEN]>);
// SAFETY: written only by `Ws2812::write` while the DMA channel is disabled.
unsafe impl Sync for DmaBuf {}
static BUF: DmaBuf = DmaBuf(UnsafeCell::new([0; LEN]));

pub struct Ws2812;

impl Ws2812 {
    pub fn new() -> Self {
        RCC.apbenr2().modify(|w| w.set_tim1en(true));
        RCC.ahbenr().modify(|w| w.set_dma1en(true));

        TIM1.cr1().write(|_| {});
        TIM1.psc().write_value(0);
        TIM1.arr().write_value(ArrCore(ARR as u32));
        // CH3 lives in CCMR2 (index 1), first half (index 0).
        TIM1.ccmr_output(1).modify(|w| {
            w.set_ocm(0, OcmGp::PWM_MODE1);
            w.set_ocpe(0, true);
        });
        TIM1.ccr(2).write_value(Ccr1ch(0));
        TIM1.ccer().modify(|w| w.set_cce(2, true));
        TIM1.bdtr().modify(|w| w.set_moe(true)); // advanced timer: main output enable
        TIM1.egr().write(|w| w.set_ug(true));
        TIM1.dier().modify(|w| w.set_ude(true));
        TIM1.cr1().modify(|w| {
            w.set_arpe(true);
            w.set_cen(true);
        });
        // Output low (CCR3 = 0) from here on; now hand PA2 to TIM1.
        gpio::alternate(GPIOA, gpio::PIN_WS2812, TIM1_AF_PA2, Pupdr::PULL_DOWN);

        DMAMUX1.ccr(CH).write(|w| w.set_dmareq_id(DMAREQ_TIM1_UP));
        let ch = DMA1.ch(CH);
        ch.cr().write(|_| {});
        ch.par().write_value(TIM1.ccr(2).as_ptr() as u32);
        ch.mar().write_value(BUF.0.get() as u32);
        ch.cr().write(|w| {
            w.set_dir(Dir::FROM_MEMORY);
            w.set_minc(true);
            w.set_psize(Size::BITS16);
            w.set_msize(Size::BITS8);
            w.set_pl(Pl::VERY_HIGH);
        });
        Self
    }

    /// Start sending one frame (GRB, MSB first). Returns false if the
    /// previous frame is still being sent (~122 µs per frame).
    pub fn write(&mut self, leds: &[Rgb; LEDS_PER_GIFT]) -> bool {
        let ch = DMA1.ch(CH);
        if ch.cr().read().en() && !DMA1.isr().read().tcif(CH) {
            return false;
        }
        ch.cr().modify(|w| w.set_en(false));
        DMA1.ifcr().write(|w| {
            w.set_gif(CH, true);
            w.set_tcif(CH, true);
            w.set_htif(CH, true);
            w.set_teif(CH, true);
        });
        // SAFETY: the channel is disabled, nothing else accesses BUF.
        let buf = unsafe { &mut *BUF.0.get() };
        let mut i = 1;
        for c in leds {
            for byte in [c.g, c.r, c.b] {
                for bit in (0..8).rev() {
                    buf[i] = if byte & (1 << bit) != 0 { T1H } else { T0H };
                    i += 1;
                }
            }
        }
        buf[0] = 0;
        buf[LEN - 1] = 0;
        ch.ndtr().write(|w| w.set_ndt(LEN as u16));
        ch.cr().modify(|w| w.set_en(true));
        true
    }
}

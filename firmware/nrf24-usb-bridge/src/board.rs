//! Every board-specific choice lives here: pins, interrupts and clocks.
//!
//! To move a signal, change the pin type below *and* the matching
//! `Peripherals` field in [`Board::take`]. The nRF IRQ pin's EXTI channel and
//! interrupt vector must match its pin number (PB1 -> EXTI1).

use embassy_stm32::exti::{self, ExtiInput};
use embassy_stm32::gpio::{Level, Output, Pull, Speed};
use embassy_stm32::mode::Blocking;
use embassy_stm32::rcc::{AHBPrescaler, APBPrescaler, Hse, HseMode, Pll, PllMul, PllPreDiv, PllSource, Sysclk};
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{Config, Peripherals, bind_interrupts, interrupt, peripherals, usb};

bind_interrupts!(pub struct Irqs {
    USB_LP_CAN1_RX0 => usb::InterruptHandler<peripherals::USB>;
    EXTI1 => exti::InterruptHandler<interrupt::typelevel::EXTI1>;
});

/// SPI clock for the nRF24 (datasheet max 10 MHz). 72 MHz APB2 / 8 = 9 MHz.
const SPI_HZ: u32 = 9_000_000;

pub type UsbDriver = usb::Driver<'static, peripherals::USB>;

/// 72 MHz SYSCLK from the 8 MHz crystal, USB at 72 / 1.5 = 48 MHz
/// (embassy selects USBPRE = /1.5 automatically for a 72 MHz PLL).
pub fn clocks() -> Config {
    let mut c = Config::default();
    c.rcc.hse = Some(Hse { freq: Hertz(8_000_000), mode: HseMode::Oscillator });
    c.rcc.pll = Some(Pll { src: PllSource::HSE, prediv: PllPreDiv::DIV1, mul: PllMul::MUL9 });
    c.rcc.sys = Sysclk::PLL1_P;
    c.rcc.ahb_pre = AHBPrescaler::DIV1;
    c.rcc.apb1_pre = APBPrescaler::DIV2; // APB1 max 36 MHz
    c.rcc.apb2_pre = APBPrescaler::DIV1; // SPI1 kernel clock 72 MHz
    c
}

pub struct Board {
    /// User LED, active low.
    pub led: Output<'static>,
    pub spi: Spi<'static, Blocking, spi::mode::Master>,
    pub csn: Output<'static>,
    pub ce: Output<'static>,
    pub irq: ExtiInput<'static, embassy_stm32::mode::Async>,
    pub usb: UsbDriver,
}

impl Board {
    pub async fn take(p: Peripherals) -> Self {
        // ---- pin map ------------------------------------------------------
        let led_pin = p.PC13;
        let (sck, miso, mosi) = (p.PA5, p.PA6, p.PA7);
        let csn_pin = p.PA4;
        let ce_pin = p.PB0;
        let (irq_pin, irq_exti) = (p.PB1, p.EXTI1);
        let (mut usb_dp, usb_dm) = (p.PA12, p.PA11);
        // -------------------------------------------------------------------

        let led = Output::new(led_pin, Level::High, Speed::Low);

        let mut sc = spi::Config::default();
        sc.frequency = Hertz(SPI_HZ);
        sc.mode = spi::MODE_0;
        // Pull MISO down so a missing module reads 0x00 and fails the probe cleanly.
        sc.miso_pull = Pull::Down;
        let spi = Spi::new_blocking(p.SPI1, sck, mosi, miso, sc);
        let csn = Output::new(csn_pin, Level::High, Speed::VeryHigh);
        let ce = Output::new(ce_pin, Level::Low, Speed::VeryHigh);
        let irq = ExtiInput::new(irq_pin, irq_exti, Pull::Up, Irqs);

        // Force re-enumeration: D+ has a fixed external 1.5 kOhm pull-up, so
        // the host never sees a disconnect across a reset. Holding D+ low for
        // 10 ms (> 2.5 us SE0 detach) makes the host drop and re-enumerate us.
        {
            let _dp = Output::new(usb_dp.reborrow(), Level::Low, Speed::Low);
            embassy_time::Timer::after_millis(10).await;
        } // dropping the Output returns PA12 to a floating input
        let usb = usb::Driver::new(p.USB, Irqs, usb_dp, usb_dm);

        Self { led, spi, csn, ce, irq, usb }
    }
}

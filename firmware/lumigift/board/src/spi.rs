//! SPI1 master for the nRF24 (mode 0, 6 MHz, 8-bit frames, software CSN),
//! exposed as an `embedded_hal::spi::SpiDevice` for the `nrf24` crate.

use core::convert::Infallible;

use embedded_hal::digital::{ErrorType as PinErrorType, OutputPin};
use embedded_hal::spi::{ErrorType, Operation, SpiDevice};

use crate::gpio::{PIN_CE, PIN_CSN, set};
use crate::pac::spi::vals::{Br, Cpha, Cpol, Ds, Frxth, Mstr};
use crate::pac::{GPIOA, RCC, SPI1};

pub fn init() {
    RCC.apbenr2().modify(|w| w.set_spi1en(true));
    SPI1.cr1().write(|_| {});
    SPI1.cr2().write(|w| {
        w.set_ds(Ds::BITS8);
        w.set_frxth(Frxth::QUARTER); // RXNE at 8 bits
    });
    SPI1.cr1().write(|w| {
        w.set_mstr(Mstr::MASTER);
        w.set_cpol(Cpol::IDLE_LOW);
        w.set_cpha(Cpha::FIRST_EDGE);
        w.set_br(Br::DIV8); // 48 MHz / 8 = 6 MHz (nRF24 max 10 MHz)
        w.set_ssm(true);
        w.set_ssi(true);
    });
    SPI1.cr1().modify(|w| w.set_spe(true));
}

#[inline]
fn xfer(b: u8) -> u8 {
    while !SPI1.sr().read().txe() {}
    SPI1.dr8().write_value(b); // 8-bit access: one frame
    while !SPI1.sr().read().rxne() {}
    SPI1.dr8().read()
}

/// SPI1 + CSN (PA11).
pub struct NrfSpi;

impl ErrorType for NrfSpi {
    type Error = Infallible;
}

impl SpiDevice for NrfSpi {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Infallible> {
        set(GPIOA, PIN_CSN, false);
        for op in ops {
            match op {
                Operation::Read(buf) => buf.iter_mut().for_each(|b| *b = xfer(0xFF)),
                Operation::Write(buf) => buf.iter().for_each(|&b| {
                    xfer(b);
                }),
                Operation::Transfer(rd, wr) => {
                    for i in 0..rd.len().max(wr.len()) {
                        let r = xfer(wr.get(i).copied().unwrap_or(0xFF));
                        if let Some(d) = rd.get_mut(i) {
                            *d = r;
                        }
                    }
                }
                Operation::TransferInPlace(buf) => buf.iter_mut().for_each(|b| *b = xfer(*b)),
                Operation::DelayNs(ns) => crate::time::delay_us(ns.div_ceil(1000)),
            }
        }
        // Every byte waited for RXNE, so the bus is idle here.
        set(GPIOA, PIN_CSN, true);
        Ok(())
    }
}

/// nRF CE (PA8).
pub struct CePin;

impl PinErrorType for CePin {
    type Error = Infallible;
}

impl OutputPin for CePin {
    fn set_low(&mut self) -> Result<(), Infallible> {
        set(GPIOA, PIN_CE, false);
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), Infallible> {
        set(GPIOA, PIN_CE, true);
        Ok(())
    }
}

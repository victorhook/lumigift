//! Small blocking nRF24L01+ driver on embedded-hal 1.0.
//!
//! Deliberately thin: register access plus the handful of operations a
//! PTX/PRX link needs. Timing (the 130 µs PLL settle, the 10 µs CE pulse,
//! the 1.5 ms power-up) is the caller's job so this crate stays
//! executor-agnostic; the required delays are documented per method.
#![no_std]

use embedded_hal::digital::OutputPin;
use embedded_hal::spi::{Operation, SpiDevice};

pub mod reg {
    pub const CONFIG: u8 = 0x00;
    pub const EN_AA: u8 = 0x01;
    pub const EN_RXADDR: u8 = 0x02;
    pub const SETUP_AW: u8 = 0x03;
    pub const SETUP_RETR: u8 = 0x04;
    pub const RF_CH: u8 = 0x05;
    pub const RF_SETUP: u8 = 0x06;
    pub const STATUS: u8 = 0x07;
    pub const OBSERVE_TX: u8 = 0x08;
    pub const RPD: u8 = 0x09;
    pub const RX_ADDR_P0: u8 = 0x0A;
    pub const RX_ADDR_P1: u8 = 0x0B;
    pub const TX_ADDR: u8 = 0x10;
    pub const RX_PW_P0: u8 = 0x11;
    pub const FIFO_STATUS: u8 = 0x17;
    pub const DYNPD: u8 = 0x1C;
    pub const FEATURE: u8 = 0x1D;
}

pub mod cmd {
    pub const R_REGISTER: u8 = 0x00;
    pub const W_REGISTER: u8 = 0x20;
    pub const R_RX_PAYLOAD: u8 = 0x61;
    pub const W_TX_PAYLOAD: u8 = 0xA0;
    pub const FLUSH_TX: u8 = 0xE1;
    pub const FLUSH_RX: u8 = 0xE2;
    pub const REUSE_TX_PL: u8 = 0xE3;
    pub const R_RX_PL_WID: u8 = 0x60;
    pub const W_ACK_PAYLOAD: u8 = 0xA8;
    pub const W_TX_PAYLOAD_NOACK: u8 = 0xB0;
    pub const NOP: u8 = 0xFF;
}

pub mod config {
    pub const MASK_RX_DR: u8 = 1 << 6;
    pub const MASK_TX_DS: u8 = 1 << 5;
    pub const MASK_MAX_RT: u8 = 1 << 4;
    pub const EN_CRC: u8 = 1 << 3;
    pub const CRCO: u8 = 1 << 2;
    pub const PWR_UP: u8 = 1 << 1;
    pub const PRIM_RX: u8 = 1 << 0;
}

pub mod feature {
    pub const EN_DPL: u8 = 1 << 2;
    pub const EN_ACK_PAY: u8 = 1 << 1;
    pub const EN_DYN_ACK: u8 = 1 << 0;
}

/// STATUS register snapshot (returned by every SPI transaction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status(pub u8);

impl Status {
    pub const RX_DR: u8 = 1 << 6;
    pub const TX_DS: u8 = 1 << 5;
    pub const MAX_RT: u8 = 1 << 4;

    pub fn rx_ready(self) -> bool {
        self.0 & Self::RX_DR != 0
    }
    pub fn tx_sent(self) -> bool {
        self.0 & Self::TX_DS != 0
    }
    pub fn max_retries(self) -> bool {
        self.0 & Self::MAX_RT != 0
    }
    /// Pipe of the payload at the head of the RX FIFO, `None` if empty.
    pub fn rx_pipe(self) -> Option<u8> {
        match (self.0 >> 1) & 0b111 {
            p @ 0..=5 => Some(p),
            _ => None,
        }
    }
    pub fn tx_full(self) -> bool {
        self.0 & 1 != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRate {
    Kbps250,
    Mbps1,
    Mbps2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Min,
    Low,
    High,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcMode {
    Off,
    OneByte,
    TwoBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub channel: u8,
    pub data_rate: DataRate,
    pub power: Power,
    pub crc: CrcMode,
    /// 3..=5
    pub addr_width: u8,
    /// 0..=15
    pub retries: u8,
    /// (n + 1) * 250 µs, n in 0..=15
    pub retry_delay: u8,
    pub dynamic_payloads: bool,
    pub ack_payloads: bool,
    pub dyn_ack: bool,
    /// Used when `dynamic_payloads` is false.
    pub payload_len: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            channel: 76,
            data_rate: DataRate::Mbps1,
            power: Power::Max,
            crc: CrcMode::TwoBytes,
            addr_width: 5,
            retries: 3,
            retry_delay: 1,
            dynamic_payloads: true,
            ack_payloads: true,
            dyn_ack: true,
            payload_len: 32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<S> {
    Spi(S),
    Pin,
    /// Register readback mismatch: chip missing or SPI wiring wrong.
    NotDetected,
    /// RX FIFO payload width > 32 (corrupt; FIFO flushed).
    BadPayloadWidth,
}

pub struct Nrf24<SPI, CE> {
    spi: SPI,
    ce: CE,
    config_reg: u8,
    /// `Some(width)` when dynamic payloads are off (R_RX_PL_WID is only
    /// meaningful with EN_DPL; static pipes use RX_PW_Px).
    static_len: Option<u8>,
}

impl<SPI: SpiDevice, CE: OutputPin> Nrf24<SPI, CE> {
    pub fn new(spi: SPI, ce: CE) -> Self {
        Self {
            spi,
            ce,
            config_reg: config::EN_CRC | config::CRCO,
            static_len: None,
        }
    }

    pub fn release(self) -> (SPI, CE) {
        (self.spi, self.ce)
    }

    // -- raw access ---------------------------------------------------------

    pub fn read_reg(&mut self, r: u8) -> Result<u8, Error<SPI::Error>> {
        let mut buf = [cmd::R_REGISTER | r, 0];
        self.spi.transfer_in_place(&mut buf).map_err(Error::Spi)?;
        Ok(buf[1])
    }

    pub fn write_reg(&mut self, r: u8, v: u8) -> Result<Status, Error<SPI::Error>> {
        let mut buf = [cmd::W_REGISTER | r, v];
        self.spi.transfer_in_place(&mut buf).map_err(Error::Spi)?;
        Ok(Status(buf[0]))
    }

    pub fn write_reg_bytes(&mut self, r: u8, data: &[u8]) -> Result<Status, Error<SPI::Error>> {
        self.command_with(cmd::W_REGISTER | r, data)
    }

    pub fn read_reg_bytes(&mut self, r: u8, out: &mut [u8]) -> Result<Status, Error<SPI::Error>> {
        let mut st = [cmd::R_REGISTER | r];
        self.spi
            .transaction(&mut [Operation::TransferInPlace(&mut st), Operation::Read(out)])
            .map_err(Error::Spi)?;
        Ok(Status(st[0]))
    }

    pub fn command(&mut self, c: u8) -> Result<Status, Error<SPI::Error>> {
        let mut buf = [c];
        self.spi.transfer_in_place(&mut buf).map_err(Error::Spi)?;
        Ok(Status(buf[0]))
    }

    fn command_with(&mut self, c: u8, data: &[u8]) -> Result<Status, Error<SPI::Error>> {
        let mut st = [c];
        self.spi
            .transaction(&mut [Operation::TransferInPlace(&mut st), Operation::Write(data)])
            .map_err(Error::Spi)?;
        Ok(Status(st[0]))
    }

    pub fn status(&mut self) -> Result<Status, Error<SPI::Error>> {
        self.command(cmd::NOP)
    }

    pub fn ce(&mut self, high: bool) -> Result<(), Error<SPI::Error>> {
        if high {
            self.ce.set_high()
        } else {
            self.ce.set_low()
        }
        .map_err(|_| Error::Pin)
    }

    // -- setup --------------------------------------------------------------

    /// Probe and fully configure the chip. Leaves it powered down with CE
    /// low and all pipes disabled; call [`Self::power_up`] next.
    pub fn init(&mut self, c: &Config) -> Result<(), Error<SPI::Error>> {
        self.ce(false)?;
        // Probe: SETUP_AW must read back what we write.
        self.write_reg(reg::SETUP_AW, 0b01)?;
        if self.read_reg(reg::SETUP_AW)? != 0b01 {
            return Err(Error::NotDetected);
        }
        self.config_reg = 0;
        self.write_reg(reg::CONFIG, 0)?;
        self.configure(c)?;
        self.write_reg(reg::EN_RXADDR, 0)?;
        self.write_reg(reg::EN_AA, 0)?;
        self.command(cmd::FLUSH_TX)?;
        self.command(cmd::FLUSH_RX)?;
        self.clear_irqs()?;
        Ok(())
    }

    /// Apply link parameters (safe to call while powered up and CE low).
    pub fn configure(&mut self, c: &Config) -> Result<(), Error<SPI::Error>> {
        self.write_reg(reg::SETUP_AW, c.addr_width.clamp(3, 5) - 2)?;
        self.write_reg(
            reg::SETUP_RETR,
            (c.retry_delay.min(15) << 4) | c.retries.min(15),
        )?;
        self.write_reg(reg::RF_CH, c.channel.min(125))?;
        let dr = match c.data_rate {
            DataRate::Kbps250 => 1 << 5,
            DataRate::Mbps1 => 0,
            DataRate::Mbps2 => 1 << 3,
        };
        let pwr = match c.power {
            Power::Min => 0,
            Power::Low => 1,
            Power::High => 2,
            Power::Max => 3,
        } << 1;
        self.write_reg(reg::RF_SETUP, dr | pwr)?;
        let mut f = 0;
        if c.dynamic_payloads {
            f |= feature::EN_DPL;
        }
        if c.ack_payloads {
            f |= feature::EN_ACK_PAY | feature::EN_DPL;
        }
        if c.dyn_ack {
            f |= feature::EN_DYN_ACK;
        }
        self.write_reg(reg::FEATURE, f)?;
        let dpl = f & feature::EN_DPL != 0;
        self.write_reg(reg::DYNPD, if dpl { 0x3F } else { 0 })?;
        self.static_len = if dpl {
            None
        } else {
            Some(c.payload_len.min(32))
        };
        for p in 0..6 {
            self.write_reg(reg::RX_PW_P0 + p, c.payload_len.min(32))?;
        }
        let crc = match c.crc {
            CrcMode::Off => 0,
            CrcMode::OneByte => config::EN_CRC,
            CrcMode::TwoBytes => config::EN_CRC | config::CRCO,
        };
        let keep = config::PWR_UP
            | config::PRIM_RX
            | config::MASK_RX_DR
            | config::MASK_TX_DS
            | config::MASK_MAX_RT;
        self.config_reg = (self.config_reg & keep) | crc;
        self.write_reg(reg::CONFIG, self.config_reg)?;
        Ok(())
    }

    /// Set a pipe's address and enable flags. Pipes 2..=5 only use `addr[0]`.
    pub fn set_rx_pipe(
        &mut self,
        pipe: u8,
        addr: &[u8],
        enabled: bool,
        auto_ack: bool,
    ) -> Result<(), Error<SPI::Error>> {
        let pipe = pipe.min(5);
        if pipe < 2 {
            self.write_reg_bytes(reg::RX_ADDR_P0 + pipe, addr)?;
        } else {
            self.write_reg(reg::RX_ADDR_P0 + pipe, addr[0])?;
        }
        let bit = 1 << pipe;
        let en = self.read_reg(reg::EN_RXADDR)?;
        self.write_reg(reg::EN_RXADDR, if enabled { en | bit } else { en & !bit })?;
        let aa = self.read_reg(reg::EN_AA)?;
        self.write_reg(reg::EN_AA, if auto_ack { aa | bit } else { aa & !bit })?;
        Ok(())
    }

    pub fn set_tx_addr(&mut self, addr: &[u8]) -> Result<(), Error<SPI::Error>> {
        self.write_reg_bytes(reg::TX_ADDR, addr)?;
        Ok(())
    }

    pub fn set_channel(&mut self, ch: u8) -> Result<(), Error<SPI::Error>> {
        self.write_reg(reg::RF_CH, ch.min(125))?;
        Ok(())
    }

    /// Set PWR_UP. Wait 1.5 ms (Tpd2stby) before raising CE.
    pub fn power_up(&mut self) -> Result<(), Error<SPI::Error>> {
        self.config_reg |= config::PWR_UP;
        self.write_reg(reg::CONFIG, self.config_reg)?;
        Ok(())
    }

    pub fn power_down(&mut self) -> Result<(), Error<SPI::Error>> {
        self.ce(false)?;
        self.config_reg &= !config::PWR_UP;
        self.write_reg(reg::CONFIG, self.config_reg)?;
        Ok(())
    }

    /// Choose which events assert the IRQ pin (active low).
    pub fn set_irq_mask(
        &mut self,
        rx_dr: bool,
        tx_ds: bool,
        max_rt: bool,
    ) -> Result<(), Error<SPI::Error>> {
        let mask = config::MASK_RX_DR | config::MASK_TX_DS | config::MASK_MAX_RT;
        self.config_reg &= !mask;
        if !rx_dr {
            self.config_reg |= config::MASK_RX_DR;
        }
        if !tx_ds {
            self.config_reg |= config::MASK_TX_DS;
        }
        if !max_rt {
            self.config_reg |= config::MASK_MAX_RT;
        }
        self.write_reg(reg::CONFIG, self.config_reg)?;
        Ok(())
    }

    // -- modes --------------------------------------------------------------

    /// Enter PRX: sets PRIM_RX and raises CE. RX is live after 130 µs.
    pub fn start_rx(&mut self) -> Result<(), Error<SPI::Error>> {
        self.config_reg |= config::PRIM_RX | config::PWR_UP;
        self.write_reg(reg::CONFIG, self.config_reg)?;
        self.ce(true)
    }

    /// Leave RX/TX: lowers CE (standby-I) and clears PRIM_RX.
    pub fn standby(&mut self) -> Result<(), Error<SPI::Error>> {
        self.ce(false)?;
        self.config_reg &= !config::PRIM_RX;
        self.write_reg(reg::CONFIG, self.config_reg)?;
        Ok(())
    }

    // -- data ---------------------------------------------------------------

    pub fn clear_irqs(&mut self) -> Result<Status, Error<SPI::Error>> {
        self.write_reg(reg::STATUS, Status::RX_DR | Status::TX_DS | Status::MAX_RT)
    }

    /// Load a TX payload (PTX, standby). Pulse CE ≥10 µs to send it, or keep
    /// CE high to stream the FIFO.
    pub fn write_tx(&mut self, data: &[u8], no_ack: bool) -> Result<Status, Error<SPI::Error>> {
        let c = if no_ack {
            cmd::W_TX_PAYLOAD_NOACK
        } else {
            cmd::W_TX_PAYLOAD
        };
        self.command_with(c, &data[..data.len().min(32)])
    }

    /// Queue a payload for the next auto-ack on `pipe` (PRX).
    pub fn write_ack_payload(
        &mut self,
        pipe: u8,
        data: &[u8],
    ) -> Result<Status, Error<SPI::Error>> {
        self.command_with(cmd::W_ACK_PAYLOAD | (pipe & 7), &data[..data.len().min(32)])
    }

    pub fn flush_tx(&mut self) -> Result<Status, Error<SPI::Error>> {
        self.command(cmd::FLUSH_TX)
    }

    pub fn flush_rx(&mut self) -> Result<Status, Error<SPI::Error>> {
        self.command(cmd::FLUSH_RX)
    }

    /// Pop one payload if available. Returns `(pipe, len)`.
    pub fn read_rx(
        &mut self,
        out: &mut [u8; 32],
    ) -> Result<Option<(u8, usize)>, Error<SPI::Error>> {
        let st = self.status()?;
        let Some(pipe) = st.rx_pipe() else {
            return Ok(None);
        };
        let len = match self.static_len {
            Some(n) => n as usize,
            None => {
                let mut w = [cmd::R_RX_PL_WID, 0];
                self.spi.transfer_in_place(&mut w).map_err(Error::Spi)?;
                w[1] as usize
            }
        };
        if len > 32 {
            self.flush_rx()?;
            return Err(Error::BadPayloadWidth);
        }
        let mut c = [cmd::R_RX_PAYLOAD];
        self.spi
            .transaction(&mut [
                Operation::TransferInPlace(&mut c),
                Operation::Read(&mut out[..len]),
            ])
            .map_err(Error::Spi)?;
        Ok(Some((pipe, len)))
    }

    /// (lost packets since last channel change, retransmits of last packet)
    pub fn observe_tx(&mut self) -> Result<(u8, u8), Error<SPI::Error>> {
        let v = self.read_reg(reg::OBSERVE_TX)?;
        Ok((v >> 4, v & 0x0F))
    }

    /// Received Power Detector: carrier above -64 dBm during the last
    /// ≥170 µs of RX.
    pub fn rpd(&mut self) -> Result<bool, Error<SPI::Error>> {
        Ok(self.read_reg(reg::RPD)? & 1 != 0)
    }

    pub fn fifo_status(&mut self) -> Result<u8, Error<SPI::Error>> {
        self.read_reg(reg::FIFO_STATUS)
    }
}

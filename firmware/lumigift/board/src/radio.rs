//! nRF24 bring-up shared by both images: PRX on the configured channel,
//! pipe 1 = broadcast, pipe 2 = unicast `[node_addr, "LGFT"]`, auto-ack,
//! dynamic payloads, ack payloads. Only RX_DR drives the IRQ pin.

use lumigift_proto::{BROADCAST_ADDR, MAX_PAYLOAD, Reply};
use nrf24::{Config, CrcMode, DataRate, Nrf24, Power, config, reg};

use crate::spi::{CePin, NrfSpi};
use crate::time::delay_us;

pub type Radio = Nrf24<NrfSpi, CePin>;

pub const PIPE_BROADCAST: u8 = 1;
pub const PIPE_UNICAST: u8 = 2;

/// CONFIG register value while running (used as a health check: an nRF
/// that browned out comes back powered down with CONFIG = 0x08).
const CONFIG_RUNNING: u8 =
    config::EN_CRC | config::CRCO | config::PWR_UP | config::PRIM_RX | config::MASK_TX_DS | config::MASK_MAX_RT;

fn link_config(channel: u8) -> Config {
    Config {
        channel,
        data_rate: DataRate::Mbps1,
        power: Power::Max,
        crc: CrcMode::TwoBytes,
        addr_width: 5,
        retries: 0, // PRX only
        retry_delay: 1,
        dynamic_payloads: true,
        ack_payloads: true,
        dyn_ack: true,
        payload_len: 32,
    }
}

/// Probe and start the radio as PRX. Retries for ~150 ms because the nRF
/// needs up to 100 ms after power-on before it answers.
pub fn start(channel: u8, node_addr: u8) -> Option<Radio> {
    let mut r = Nrf24::new(NrfSpi, CePin);
    let mut ok = false;
    for _ in 0..15 {
        if r.init(&link_config(channel)).is_ok() {
            ok = true;
            break;
        }
        delay_us(10_000);
    }
    if !ok {
        return None;
    }
    let _ = r.set_rx_pipe(PIPE_BROADCAST, &BROADCAST_ADDR, true, true);
    let _ = r.set_rx_pipe(PIPE_UNICAST, &[node_addr], true, true);
    let _ = r.set_irq_mask(true, false, false);
    let _ = r.power_up();
    delay_us(1500); // Tpd2stby
    let _ = r.flush_rx();
    let _ = r.flush_tx();
    let _ = r.clear_irqs();
    let _ = r.start_rx();
    delay_us(130); // RX settling
    Some(r)
}

/// Is the chip still configured as we left it?
pub fn healthy(r: &mut Radio) -> bool {
    matches!(r.read_reg(reg::CONFIG), Ok(v) if v == CONFIG_RUNNING)
}

/// Change the unicast address (pipe 2) while running.
pub fn set_node_addr(r: &mut Radio, node_addr: u8) {
    let _ = r.ce(false);
    let _ = r.set_rx_pipe(PIPE_UNICAST, &[node_addr], true, true);
    let _ = r.ce(true);
}

pub fn set_channel(r: &mut Radio, channel: u8) {
    let _ = r.ce(false);
    let _ = r.set_channel(channel);
    let _ = r.ce(true);
}

/// Replace the pending ack payload with exactly one `reply`.
pub fn preload(r: &mut Radio, reply: &Reply) {
    let mut buf = [0u8; MAX_PAYLOAD];
    let n = reply.encode(&mut buf);
    let _ = r.flush_tx();
    let _ = r.write_ack_payload(PIPE_UNICAST, &buf[..n]);
}

/// Power the radio down (CE low, PWR_UP = 0) before leaving the image.
pub fn shutdown(r: &mut Radio) {
    let _ = r.flush_tx();
    let _ = r.power_down();
}

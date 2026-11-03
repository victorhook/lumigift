//! Link configuration.

use std::path::PathBuf;
use std::time::Duration;

use nrf24_usb_proto::{DataRate, Power, RadioConfig, features};

/// Radio parameters shared by the bridge and every gift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioSettings {
    /// 0..=125 (2400 + n MHz). Gifts default to 76.
    pub channel: u8,
    pub data_rate: DataRate,
    pub power: Power,
    /// Hardware auto-retransmits per unicast (0..=15).
    pub retries: u8,
    /// Auto-retransmit delay, `(n + 1) * 250 µs`. The default is 1 (500 µs):
    /// per the nRF24L01+ datasheet, 250 µs only fits ack payloads of ≤ 5 bytes
    /// at 1 Mbps, and Status replies are 27 bytes.
    pub retry_delay: u8,
}

impl Default for RadioSettings {
    fn default() -> Self {
        Self {
            channel: lumigift_proto::DEFAULT_CHANNEL,
            data_rate: DataRate::Mbps1,
            power: Power::Max,
            retries: 3,
            retry_delay: 1,
        }
    }
}

impl RadioSettings {
    pub fn to_radio_config(&self) -> RadioConfig {
        RadioConfig {
            channel: self.channel,
            data_rate: self.data_rate,
            power: self.power,
            crc_len: 2,
            addr_width: 5,
            retries: self.retries,
            retry_delay: self.retry_delay,
            features: features::DYN_PAYLOAD | features::ACK_PAYLOAD | features::DYN_ACK,
            payload_len: 32,
        }
    }
}

/// Where host-side gift metadata (names, colour tags, notes) is stored.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum MetadataLocation {
    /// `~/.config/lumigift/gifts.json` (platform config dir).
    #[default]
    Default,
    Path(PathBuf),
    /// Keep in memory only.
    Disabled,
}

/// OTA engine parameters.
#[derive(Debug, Clone)]
pub struct OtaOptions {
    /// Chunk broadcast pace, chunks/s. `0` (default) = automatic: the rate
    /// the gifts can program once the image's pages are erased (from
    /// `double_word_time`: ≈2,000 chunks/s on real gifts).
    pub chunks_per_s: f32,
    /// Max query/rebroadcast rounds after the first pass.
    pub max_rounds: u8,
    /// How long to wait for targets to report `IN_BOOTLOADER`.
    pub bootloader_timeout: Duration,
    /// How long to wait for targets to come back running the new image.
    pub boot_timeout: Duration,
    /// Lower the rate for repair rounds when the first pass lost much
    /// (the gifts' RX rings overflowed or the link is poor).
    pub adaptive: bool,
    /// Skip targets that already run this exact image (same `fw_version`
    /// and app CRC). [`Link::start_ota_with`](crate::Link::start_ota_with)
    /// can force an update anyway.
    pub skip_up_to_date: bool,
    /// Flash timings: the pause after `OtaBegin` while the gifts erase, and
    /// the automatic chunk rate.
    pub page_erase_time: Duration,
    pub double_word_time: Duration,
}

impl Default for OtaOptions {
    fn default() -> Self {
        Self {
            chunks_per_s: 0.0,
            max_rounds: 20,
            bootloader_timeout: Duration::from_secs(4),
            boot_timeout: Duration::from_secs(4),
            adaptive: true,
            skip_up_to_date: true,
            page_erase_time: Duration::from_millis(22),
            // Effective, including the bootloader's per-double-word work:
            // measured ~2,200 chunks/s sustained on a real STM32C011 (the
            // datasheet's 85 µs alone would allow ~3,500).
            double_word_time: Duration::from_micros(140),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub radio: RadioSettings,
    /// Show frame period (10 ms = 100 Hz).
    pub show_period: Duration,
    /// `Sync` beacon period while the show output is `Idle`.
    pub idle_sync_period: Duration,
    /// Target interval between polls of one online gift.
    pub poll_interval: Duration,
    /// Interval between polls of an offline gift (so it is noticed coming back).
    pub offline_poll_interval: Duration,
    /// Global cap on telemetry polls per second.
    pub max_polls_per_s: f32,
    /// A gift is offline after this long without a successful poll.
    pub offline_after: Duration,
    /// Max `Tx` in flight at the bridge (also capped by its `tx_queue_depth`).
    /// Small values keep show frames from queueing behind a backlog.
    pub max_in_flight: usize,
    /// Max unicast `Tx` in flight (each can take ~2.5 ms with retries).
    pub max_unicast_in_flight: usize,
    /// No unicast is started this close before a show tick.
    pub tick_guard: Duration,
    /// Run the full discovery sweep on connect.
    pub auto_discover: bool,
    /// The background sweep covers all unknown addresses in about this time.
    pub background_sweep_period: Duration,
    /// Each gift's identity is re-checked (collision detection) this often.
    pub reidentify_interval: Duration,
    pub metadata: MetadataLocation,
    /// History ring length (samples) and sample interval.
    pub history_len: usize,
    pub history_interval: Duration,
    /// Event log length.
    pub log_len: usize,
    pub ota: OtaOptions,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            radio: RadioSettings::default(),
            show_period: Duration::from_millis(10),
            idle_sync_period: Duration::from_millis(100),
            poll_interval: Duration::from_millis(50),
            offline_poll_interval: Duration::from_millis(400),
            max_polls_per_s: 250.0,
            offline_after: Duration::from_secs(1),
            max_in_flight: 4,
            max_unicast_in_flight: 2,
            tick_guard: Duration::from_micros(1500),
            auto_discover: true,
            background_sweep_period: Duration::from_secs(8),
            reidentify_interval: Duration::from_secs(20),
            metadata: MetadataLocation::Default,
            history_len: 480,
            history_interval: Duration::from_millis(500),
            log_len: 500,
            ota: OtaOptions::default(),
        }
    }
}

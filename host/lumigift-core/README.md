# lumigift-core

The link manager between the GUI/CLI and the gifts: radio scheduler, gift
registry and telemetry, discovery and address-collision handling, commands, the
OTA engine, bridge clock sync, and a faithful simulator so everything runs
without hardware.

```toml
lumigift-core = { path = "../lumigift-core" }
```

## Quick start

```rust
use lumigift_core::{sim::SimConfig, Link, LinkConfig, ShowOutput, SlotAssign, Target};
use lumigift_core::{MusicFrame, Rgb};
use std::time::Duration;

// Hardware: let link = Link::open_usb(LinkConfig::default(), None);
let (link, sim) = Link::simulated(LinkConfig::default(), SimConfig::demo(10));

// Discovery runs by itself; wait for the first sweep (≈1 s) if you need it.
link.wait_for(Duration::from_secs(5), |s| s.discovery.initial_sweep_done);

// Snapshot: lock-free, published ~20×/s. Read it every GUI frame.
let snap = link.snapshot();
for g in &snap.gifts {
    println!("{:<10} slot {:?} addr {} {} mV, link {:.0}%, bcast {:?}%",
             g.label(), g.slot, g.node_addr, g.vbat_mv, g.poll_success_pct, g.bcast_rx_pct);
}

// Show output: latest value wins; call it at up to 100 Hz from the audio engine.
link.set_show(ShowOutput::Music(MusicFrame { effect: 1, brightness: 200, ..Default::default() }));
link.set_show(ShowOutput::Solid(vec![Rgb::new(255, 0, 0); 10])); // index = slot
link.set_show(ShowOutput::Idle);                                    // Sync beacon only

// Commands return a Pending<T>; poll it from the UI loop or block on it.
let mut p = link.blink(Target::All, Rgb::new(0, 0, 255), 3, Duration::from_millis(300));
if let Some(result) = p.try_take() { /* finished: Result<CmdReport, LinkError> */ }
link.auto_assign_slots(SlotAssign::KeepExisting).wait()?;

// Simulator only: what the real LEDs would show right now.
for g in sim.gifts() { println!("{:?} {:?}", g.slot, g.leds); }
# Ok::<(), lumigift_core::LinkError>(())
```

## API overview

| What | Call |
|---|---|
| Start | `Link::open_usb(cfg, serial)`, `Link::simulated(cfg, SimConfig)`, `Link::start(cfg, connector)` (any `nrf24_usb::Transport`) |
| Observe | `snapshot() -> Arc<Snapshot>`, `wait_for(timeout, pred)`, `subscribe_log() -> Receiver<LogEvent>`, `is_connected()` |
| Show | `set_show(ShowOutput)`, `show()` |
| Clock | `clock() -> ClockSync` (`bridge_now_us()`, `from_instant()`, `to_instant()`, `unwrap(u32)`, `raw_to_instant(u32)`), `bridge_now_us()` |
| Discovery | `rescan() -> Pending<usize>` (initial + background sweeps are automatic) |
| Commands | `blink`, `identify`, `set_slot`, `auto_assign_slots`, `reboot(target, into_bootloader)`, `set_config(target, key, value)`, `set_channel(ch)` |
| Metadata | `rename(uid, Option<String>)`, `update_meta(uid, MetaUpdate)` (stored in `~/.config/lumigift/gifts.json`) |
| OTA | `start_ota(image_bytes, Vec<Uid>) -> Pending<OtaReport>` (skips targets already running the image), `start_ota_with(.., force)`, `cancel_ota()`, `ota_running()`; progress in `Snapshot::ota`; `OtaReport` has `skipped`, `phase_times`, `chunk_rate` |
| Rescue | `rescue(duration) -> Pending<RescueReport>`: broadcast `BootHold` on the default channel while the user power-cycles unreachable gifts; lists the gifts then in their bootloader |
| Gift references | `Snapshot::resolve("s3" / "@17" / "3f2a91" / "kitchen" / "all")` |
| Shutdown | `shutdown()` or drop the `Link` |

`Target` is `Gift(uid)`, `Gifts(vec)` or `All` (every online gift); `Uid` and
`Vec<Uid>` convert into it. Multi-gift commands return a `CmdReport` with the
per-gift outcome. When a `Pending` resolves, the snapshot already reflects the
outcome.

### Snapshot contents

* `bridge`: state (`Connecting` / `Connected` / `Disconnected(reason)`; the link
  retries every second, so unplug/replug recovers by itself), bridge `Info` and
  `Stats`, radio settings, clock sync state, ping RTT.
* `gifts: Vec<GiftView>`: UID, metadata, node address, slot, fw/boot versions,
  `in_bootloader`, `online` (offline after 1 s without a successful poll), last
  status (uptime, vbat, flags, reset reason, resets, sync error, temperature),
  host-detected reboots, poll success %, average retries, broadcast reception %
  and losses (last 5 s), poll counters, `address_conflict`, and
  `history: Arc<Vec<HistorySample>>` (2 Hz, 4 min: link %, retries, vbat, sync
  error, broadcast %, online) for sparklines. `NaN` in a history field means no
  data.
* `link`: show frames/s, show packets/s, polls/s, TX totals/failures, dropped
  show packets, in-flight counts.
* `discovery`, `ota: Option<OtaProgress>` (per-target phase/percent/missing/
  retries/error), `events: Arc<Vec<LogEvent>>` (bounded log), `show_paused`.

## Threading model

* **`lumigift-link`** (scheduler) owns the transport. Every 10 ms it broadcasts
  the show frame (NO_ACK + STAMP, incrementing seq), then fills the remaining
  air time with unicast exchanges requested by operations, broadcast jobs (OTA
  chunks), and round-robin telemetry polls. At most `max_in_flight` (4) `Tx` are
  queued at the bridge and at most `max_unicast_in_flight` (2) unicasts, and no
  unicast starts within 1.5 ms of a show tick, so show frames never wait behind a
  backlog. A show packet a tick could not send is dropped in favour of the next
  tick's.
* **`lumigift-discovery`**: initial sweep of addresses 1..=254, slow background
  sweep (~8 s per pass), re-identification after reboots and every 20 s, and
  collision resolution via broadcast `SetNodeAddr`.
* **One thread per command / OTA**: straight-line code that blocks on the
  scheduler through channels.
* **`nrf24-usb-rx`**: USB reader with 8 queued IN transfers (hardware only).

GUI-facing calls (`snapshot`, `set_show`, `Pending::try_take`) never block.

## Simulator

`sim::SimTransport` / `Link::simulated` emulate the bridge and N gifts in
real time, faithfully enough that a simulated firmware update takes about as
long as on hardware (`OtaReport::duration` is the estimate):

* **Bridge**: serial radio queue (24 deep), NO_ACK streaming as in the bridge
  firmware (back-to-back packets, run cap, PLL settle per run), auto-ack
  timing (settle, turnaround, ack payload, IRQ service, ARD per retry), STAMP,
  exactly one TxDone per Tx, 0.5 ms USB latency, a clock that wraps 3 s after
  start.
* **Radio**: per-gift loss in both directions per attempt (optionally bursty,
  `SimConfig::loss_burstiness`), ack collisions with capture.
* **Gifts** (`src/sim/gift.rs`): PRX with a 3-deep RX FIFO, duplicate
  suppression, the one-reply-preloaded ack-payload semantics, Status counters,
  a draining battery, random reboots, config keys. The **bootloader runs the
  firmware's own logic** (`lumigift-logic`: boot decision, `BootShared`,
  recovery mode, OTA receiver, channel plan, RX rings) with flash timings
  (22 ms page erase, 85 µs per double word, CRC unit), the RAM drain during
  flash work, the 150 ms `BootHold` window on the default channel, the IWDG,
  radio start-up delays (100 ms after power-on).

LEDs are rendered with `lumigift_fx::Renderer`. `SimHandle` exposes `gifts()`
(incl. LED colours, channels, boot attempts, OTA state) and fault injection:
`set_loss_pct`, `set_powered`, `power_cycle`, `reboot`, `set_node_addr`,
`add_gift`, `set_reboot_rate`, `transport()` (replug), `set_app_fault`
(crash loop / hang / deaf radio), `set_config_channel`,
`schedule_power_cut` (mid-erase, after n chunks, after `Verified`),
`glitch_flash_write`. `SimConfig::new(n)` is quiet (2% loss),
`SimConfig::demo(n)` is livelier. `sim::build_test_image(fw, len, seed)` makes
valid firmware images.

## Protocol assumptions (firmware authors, please check)

* **OTA `image_crc`** in `OtaBegin` = CRC-32 of the whole image (`image_len`
  bytes: 256-byte header block + app). The bootloader verifies it, the header,
  the vector table and `app_crc` as soon as the last chunk is stored. The last
  chunk is padded with `0xFF`.
* **Channel change** uses the new `config_key::CHANNEL` (added to
  `lumigift-proto`): the gift replies on the old channel and switches
  `CHANNEL_SWITCH_DELAY_MS` (1 s) later. The host then moves the bridge.
* The host never uses `seq == 0`, so a freshly booted gift (echo 0) can't fake a
  match. A command's reply is collected with a Poll sent after the command's
  TxDone (≥ 1 USB round trip later), which gives the gift time to preload.
* `Reboot` counts as done when the command is acked (the gift may reset before
  preloading a reply).
* Default auto-retransmit delay is 500 µs, not 250 µs: per the nRF24L01+
  datasheet, 250 µs only fits ack payloads of ≤ 5 bytes at 1 Mbps and Status
  replies are 27 bytes. Configurable in `RadioSettings`.

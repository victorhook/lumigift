# Lumigift architecture

```
                        USB full-speed (vendor bulk)                2.4 GHz nRF24, 1 Mbps, ch 76
 ┌──────────────────────┐   nrf24-usb-proto   ┌───────────────────┐   lumigift-proto   ┌────────────────┐
 │ host/                │ ◄─────────────────► │ firmware/          │ ◄────────────────► │ firmware/       │ ×N
 │  lumigift-studio (UI)│                     │  nrf24-usb-bridge  │                    │  lumigift       │
 │  lumigift-cli        │                     │  STM32F103C8       │                    │  STM32C011F6    │
 │  lumigift-core       │                     │  + nRF24L01+PA/LNA │                    │  boot + app     │
 │  lumigift-audio      │                     └───────────────────┘                    └────────────────┘
 │  nrf24-usb (driver)  │
 └──────────────────────┘
        shared no_std crates (crates/): nrf24, nrf24-usb-proto, lumigift-proto, lumigift-fx
```

| Crate | Where it runs | Role |
|---|---|---|
| `crates/nrf24-usb-proto` | bridge + host | Generic host↔bridge wire protocol. Knows nothing about Lumigift. |
| `crates/nrf24` | bridge + gifts | Blocking nRF24L01+ register driver (embedded-hal 1.0). |
| `crates/lumigift-proto` | gifts + host | Over-the-air messages, flash layout, image header, CRC-32, config record. |
| `crates/lumigift-fx` | gifts + host | Effect renderer. Same code drives real LEDs and the 3D preview. |
| `firmware/nrf24-usb-bridge` | STM32F103C8 | USB ⇄ nRF24 bridge, reusable for other projects. |
| `firmware/lumigift` | STM32C011F6 | Radio bootloader (10 KiB, write-protected) + app (≤ 19.75 KiB). |
| `host/nrf24-usb` | PC | Driver for the bridge (nusb, no libusb). |
| `host/lumigift-core` | PC | Link scheduler, gift registry/telemetry, discovery, OTA engine, simulator. |
| `host/lumigift-audio` | PC | Decoding, playback clock, offline analysis, show model, show engine. |
| `host/lumigift-cli` | PC | Headless commands (status, blink, ota, rescue, play). |
| `host/lumigift-studio` | PC | egui desktop app with live monitor + 3D show preview/editor. |

## Gift hardware (lumigift-rev-a)

STM32C011F6P6 (TSSOP-20, Cortex-M0+, 48 MHz HSI48, 32 KiB flash in 2 KiB pages, 6 KiB RAM).

| Signal | Pin | Notes |
|---|---|---|
| WS2812B data (4 LEDs, GRB) | PA2 | TIM1_CH3 |
| Red status LED | PB6 | active high |
| VBAT sense | PA3 | ADC_IN3; single AAA cell (≈0.9–1.6 V) direct to the pin |
| nRF CSN | PA11 | the package pin is shared "PA11 [PA9]" (check the SYSCFG remap) |
| nRF CE | PA8 | |
| nRF SCK / MISO / MOSI | PA5 / PA6 / PA7 | SPI1 |
| nRF IRQ | PA4 | active low, open drain from nRF |
| UART TX / RX (debug) | PC14 / PB7 | USART1, optional |
| SWD | PA13 / PA14 | ST-Link via probe-rs |

Power: AAA → boost to 5 V (LEDs) and ME6215 3.3 V LDOs (MCU, radio). A full-white
gift would pull far more than an AAA boost converter can give, so every frame passes
through `lumigift_fx::power_limit` (default budget 250 mA at 5 V).

## Radio link

See the module docs of `lumigift-proto` for the full message list. Summary:

* Every gift is a PRX at all times. Pipe 1 = broadcast `[0x00,"LGFT"]`, pipe 2 =
  unicast `[node_addr,"LGFT"]`, both auto-ack + dynamic payload. The bridge sends
  broadcasts with NO_ACK, so nobody replies to them.
* **Downlink show stream:** the host broadcasts one show frame every 10 ms
  (Music/Solid/Leds/Blackout). Each one carries a sequence number and the bridge's µs
  timestamp, which the bridge stamps at TX time. Gifts count sequence gaps
  (`bcast_lost`) and keep their clock locked to the bridge.
* **Uplink:** only through ack payloads. After every unicast packet a gift flushes its
  TX FIFO and preloads one reply. The host sends a command, then a `Poll`, and
  reads the reply from the Poll's ack, matching `echo` with `seq`. The host polls
  gifts round-robin between show frames, so every gift reports `Status` (uptime,
  battery, link counters, reset reason, fw version) several times per second.
* **Discovery:** the host sweeps node addresses 1..=254 with `Poll`. When two gifts
  share an address, `GetIdentity` tells them apart and a broadcast `SetNodeAddr`
  (matched by UID) moves one of them.
* **Liveness:** a gift whose polls fail for 1 s is marked offline. The gift
  itself turns its LEDs off after `idle_timeout` with no show frames. An IWDG
  watchdog resets a hung gift.

## Firmware update (OTA)

Goals, in this order: a failed or interrupted update never bricks a gift
(worst case the app is broken, the bootloader always works and can be
re-flashed over the air), and updates are as fast as the hardware allows.

The flash layout is in `lumigift_proto::layout`:

```
0x0800_0000  bootloader  10 KiB   never written over the air; write-protected (WRP area A)
  0x0800_27F0  boot info block (16 B)
0x0800_2800  app region  20 KiB   [ImageHeader padded to 256 B][app linked at 0x0800_2900]
0x0800_7800  config       2 KiB   append-only ConfigRecord log (node_addr, slot, channel, limits)
RAM 0x2000_17F0  BootShared 16 B  shared between bootloader and app, survives warm reset
```

### Never brick

* **Only the bootloader receives images**, and nothing can write it: the
  flash driver refuses any erase/program below the image's *write floor*
  (a linker-script symbol: app region for the bootloader, config page only
  for the app; never below the app region whatever the symbol says), and
  the bootloader write-protects its own 5 pages through the option bytes
  (WRP area A) on its first boot. If programming the option bytes fails it
  carries on unprotected: a RAM flag in `BootShared` (kept across the
  option-byte reload reset, cleared at power-on) limits the attempt to
  once per power cycle, so this can never become a reset loop.
* **A partial image never runs.** Pages are erased in order starting with
  page 0, which holds the old header, so the old image stops validating at
  the first erase. The bootloader boots only an image whose header CRC,
  board revision, vector table (SP in RAM, Thumb reset vector inside the
  app), app CRC and (for a fresh update) whole-image CRC all check out;
  otherwise it stays in recovery mode, an OTA receiver, forever.
* **A broken app ends in recovery.** The IWDG (~2 s) runs from the first
  bootloader instruction and across the jump; the bootloader counts boot
  attempts in `BootShared`, the app clears the count after 5 s healthy, and
  at 3 attempts the bootloader stays in recovery. So crash loops (HardFault →
  reset) and hangs (IWDG reset) both end there within a few seconds.
* **The bootloader is always reachable.** After every reset it listens for
  `BootHold` for 150 ms on the **default channel** (76), whatever the
  app-written config says, so an app that runs but whose radio is useless
  (deaf, wrong channel, wrong address) is rescued by broadcasting `BootHold`
  while power-cycling the gift (`Link::rescue`, `lumigift rescue`). In
  recovery the bootloader alternates between the configured and the default
  channel every second until it hears any Lumigift packet, then stays on that
  channel (until 3 s of silence). A corrupt config record falls back to the
  defaults. Node addresses: `EnterBootloader` and discovery find gifts by
  address, `SetNodeAddr` (broadcast, matched by UID) moves a gift, and a
  discovery sweep on the default channel finds a recovery-mode gift with an
  unknown address. After a verified update the bootloader stores the channel
  the update came on, so the new app is reachable too.

Tests: `firmware/lumigift/logic` (power cut before every few flash
operations of an update, corrupt images, crash loops / hangs, channel plan,
stale sessions, repeated Begin, host vanishing) and
`host/lumigift-core/tests/ota.rs` (the same through the whole host stack,
with power cuts mid-erase / mid-transfer / after `Verified`, flash write
errors, a deaf app rescued with `BootHold`, a wrong stored channel).

### Protocol

1. **Enter**: one `EnterBootloader { addrs }` broadcast (×5) carries every
   target's node address (30 per packet); apps listed reboot at once with
   `REQ_STAY_IN_BOOT` (no 150 ms window). The host polls each target until
   its `Status` says `IN_BOOTLOADER` (≈ 6 ms after the reset), and falls back
   to unicast `Reboot { into_bootloader }` after 150 ms.
2. `OtaBegin { session, image_len, image_crc }` (×3). The bootloader erases
   only the `ceil(image_len / 2 KiB)` pages the image covers, in order,
   in the background (ahead of time when idle, or on demand when a chunk
   for an unerased page comes up), and buffers chunks meanwhile.
3. `OtaChunk { session, index, data[24] }` for every chunk, in order, paced
   at the rate the gifts' flash absorbs (see below).
4. `OtaQuery { session, window }` → `OtaBitmap { state, window, missing_count,
   bits[24] }`: the received bits of the first 192-chunk window at or after
   `window` that has holes (chunks still in the RX ring count as received).
   The host asks only for windows with holes and re-broadcasts the union of
   every target's missing chunks, repeating until all are `Verified`.
5. The bootloader verifies the image as soon as the last chunk is stored
   (`Verified`/`Failed`; `OtaCommit` asks explicitly). The poll that
   collects `Verified` makes it jump to the app at once (200 ms later if
   nobody asks). The app brings its radio up before anything else, so the
   host confirms the new `fw_version` by polling the gift's address ~5 ms
   later. Targets that report `Failed`, a lost session (power cut) or no
   answer for a while are restarted (same session, full stream) up to twice.

Targets already running the image (same `fw_version` and app CRC, which
`Identity` now reports) are skipped unless forced. `Status.bcast_lost` in
recovery counts RX-ring overflows.

### Speed

The bottleneck is the gifts' flash, not the radio:

| | per chunk (24 B) | chunks/s |
|---|---:|---:|
| air, 1 Mbps NO_ACK run (28 B payload = 297 µs + run overhead) | ~320 µs | ~3,100 |
| air, 2 Mbps | ~160 µs | ~6,300 |
| gift flash: 3 double words × 85 µs + CPU, plus 22 ms per 2 KiB page (85 chunks) | ~540 µs | ~1,850 |

So a 15.8 KB image needs at least ~0.36 s of flash time (8 page erases +
1,974 double words) whatever the air rate. To not lose that time:

* **Erase overlaps reception, programming overlaps reception.** The
  bootloader's erase/program sequences and the radio drain run from RAM
  (`.data.ramfunc.*`); while the flash is busy the CPU keeps moving packets
  from the 3-deep nRF RX FIFO into RAM rings (4 unicast + 80 broadcast slots,
  2.8 KiB), so a 22-40 ms page erase loses nothing, and unicast queries are
  answered before buffered chunks.
* The host streams the first pass at the flash rate (from the image's page
  and non-0xFF double-word counts, ≈1,800 chunks/s); repair rounds run at
  ~2,800 chunks/s (all pages are erased by then), or slower after a very
  lossy first pass. Measured in the simulator, streaming faster (up to the
  bridge's limit) gives the same total time, only more repairs.
* **2 Mbps: not used.** It would only shorten the air time, which is not the
  limit (flash-bound at ~1,850 chunks/s), while costing ~3 dB of sensitivity
  and adding a rate switch to the never-brick path.
* **Chunk size: kept at 24 bytes.** 28 data bytes per 32-byte packet would
  save ~5 % air per byte (irrelevant when flash-bound) but would split
  double words across chunks, which out-of-order repair chunks make
  awkward to program exactly once.

Simulated wall-clock time to update 10 gifts with the current 15.8 KB app
image (`cargo test -p lumigift-core --test ota -- timing --nocapture`; the
simulator runs in real time against the real host code, with the bridge's
air timing, USB latency, the gifts' flash timings, RX FIFO/ring limits and
random loss):

| loss | before (8 KiB bootloader, erase-all, first-10-missing list, 1,500 chunks/s) | now |
|---:|---:|---:|
| 0 % | 1.6 s | 0.48 s |
| 5 % | 2.1 s | 0.67 s |
| 20 % | 2.7 s | 1.1 s |

### Boot decision

* No valid image (bad header, or app CRC mismatch) → stay in recovery mode.
* `BootShared.request == REQ_STAY_IN_BOOT` → stay.
* `boot_attempts >= 3` (the app kept crashing before marking itself healthy) → stay.
* Otherwise listen 150 ms for `BootHold` on the default channel, then
  increment `boot_attempts` and jump to the app.

## Clocks

The bridge's free-running µs clock is the system time base. The host maps it to
its own monotonic clock with `Ping`/`Pong` (bridge timestamps also arrive in
`TxDone`/`Rx`). Gifts lock to it from show-frame timestamps. `lumigift-fx`
renders from synced time, so all gifts animate in lockstep.

## Audio → light pipeline

```
 AudioSource ──► decoder ──► Player (cpal) ──► speakers
     │                           │ position(): the audio being heard right now
     ▼                           ▼
  Analysis (offline for files:    ShowEngine.frame_at(position + radio_latency)
  bands, onsets, beat grid,              │
  downbeats, sections, drops)            ▼
     │                          MusicFrame @100 Hz ──► lumigift-core ──► bridge ──► gifts
     └──► Show (cues: effect/palette/brightness over time; auto-generated, editable)
```

* File sources first: the whole track is analysed up front, so the engine can
  look ahead and line the lights up exactly with the sound.
* Later sources fit behind the same `AudioSource` + feature-frame interface:
  PipeWire loopback capture of any app (e.g. Spotify) with a matching
  playback delay, and Spotify Connect via librespot once it works again.

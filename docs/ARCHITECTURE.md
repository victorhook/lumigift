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
| `host/lumigift-cli` | PC | Headless commands (status, blink, ota, rescue, sleep, wake, play). |
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
  (Music/Solid/Leds/Blackout), or one Music frame per group ("layers", see
  [Lighting](#lighting-generic-looks-and-groups)). Each one carries a sequence number and the bridge's µs
  timestamp, which the bridge stamps at TX time. Gifts count sequence gaps
  (`bcast_lost`) and keep their clock locked to the bridge.
* **Uplink:** only through ack payloads. After every unicast packet a gift flushes its
  TX FIFO and preloads one reply. The host sends a command, then a `Poll`, and
  reads the reply matching `echo` with `seq`. A gift often handles the packet
  inside the nRF's ~130 µs ack turnaround (measured on hardware), so the reply
  may already ride on the command's own ack; the host checks that ack first and
  only polls if needed. Gifts keep a command reply for the next 2 polls
  (`REPLY_POLLS`, app and bootloader alike), so a poll never overwrites a reply
  that has not been delivered. The simulator models both timings
  (`SimConfig::fast_reply_pct`, 50 % by default). The host polls
  gifts round-robin between show frames, so every gift reports `Status` (uptime,
  battery, link counters, reset reason, fw version) several times per second.
* **Discovery:** the host sweeps node addresses 1..=254 with `Poll`. When two gifts
  share an address, `GetIdentity` tells them apart and a broadcast `SetNodeAddr`
  (matched by UID) moves one of them.
* **Liveness:** a gift whose polls fail for 1 s is marked offline. The gift
  itself turns its LEDs off after `idle_timeout` with no show frames. An IWDG
  watchdog resets a hung gift.

## Power / doze

Constants in `lumigift_proto::doze`. A dozing gift has its LEDs off and its
receiver on for only `LISTEN_US` (2 ms) every `PERIOD_MS` (1 s, on its ±10 %
LSI clock, so each gift has its own phase and period). Outside those windows
it acks and hears nothing. In a window, `Broadcast::Wake` or a unicast to its
own address wakes it. Other broadcasts (show frames, `Sync`) are ignored, and
it keeps dozing. A gift dozes on `Broadcast::Sleep`, or by itself after
`AUTO_AFTER_MS` (60 s) without any decodable host packet. A connected host
never lets that happen, since it sends `Sync` beacons and polls at all times.

Gift side (`firmware/lumigift/board/src/power.rs`, `app`):

* **Awake:** the main loop ends in `WFI`. A 1 kHz TIM14 compare tick and the
  radio IRQ wake it (the STM32C011 draws ~1 mA asleep vs ~3 mA running at
  48 MHz). Every mA at 3.3 V costs ~4 mA from the 1.5 V cell (5 V boost, then
  the 3.3 V LDO), so the nRF receiving all the time (13.5 mA) dominates.
* **Doze:** LEDs black (the last WS2812 frame must be out before the clock
  changes), ADC and its regulator off, then the system clock moves to the
  32 kHz LSI with HSI48 off (~80 µA, like Stop mode, but GPIOs and timers keep
  working, so no RTC or clock-restore dance). With interrupts masked, TIM16
  one-shots end each `WFI`; a window is only GPIO: CE high for `LISTEN_US`,
  CE low, look at the IRQ pin (the nRF sits in standby-I, 26 µA, in between).
  The IWDG (~2 s) is fed every period. After a window that caught something
  the gift returns to 48 MHz, handles the packets normally and, unless one was
  `Wake` or a unicast, dozes on. The µs clock is advanced by the doze time
  (`time::resume`).
* **Measured** (1.5 V bench supply, LEDs off): 106 mA before (receiver always
  on, CPU busy-looping), **12 mA dozing**, mostly the four WS2812Bs' quiescent
  current and the boost converter.

Host side (`lumigift-core`):

* **`Link::sleep_all`** broadcasts `Sleep` (×5), then polls each online gift
  once. Gifts that stop answering are marked asleep, and those that still
  answer get another round (3 rounds at most). Gifts in their bootloader
  don't doze.
* **Asleep is a state of its own** (`GiftView::asleep`, `online == false`).
  The host marks it, and the gift never reports it. Asleep gifts are not
  polled, swept or re-identified, because they can't answer and a stray
  unicast landing in a listen window would wake them. Any acked packet from
  a gift clears it ("awake" in the log).
* **`Link::wake_all`** sends `Wake` back to back for `WAKE_BURST_MS` (1.3 s,
  more than one full period of the slowest clock), measured in time rather
  than packets. The bridge sends ~8,000 one-byte packets/s. The burst jobs
  are queued two deep and allowed through the scheduler's `tick_guard`
  (`Radio::broadcast_gapless`), so no gap is long enough to swallow a 2 ms
  window. Telemetry polls pause during the burst so online gifts don't look
  offline. Afterwards every asleep/offline gift is polled (an answer marks it
  online), the rest count as offline, and a discovery sweep finds gifts this
  host never saw awake. `Snapshot::waking` is set meanwhile.
* **Automatic wakes:** on connecting to a bridge, before the first sweep
  (`LinkConfig::wake_on_connect`, default on), because gifts may have dozed
  while no host ran. At the start of an OTA when any target is not online.
  When the show output goes from Idle/Blackout to an active look while known
  gifts are asleep. Only that transition counts, so a look that was already
  running when "Sleep all" was pressed does not undo it.
* CLI: `lumigift sleep`, `lumigift wake`, `--no-wake`. `status` shows
  `asleep`. Studio: "Sleep all" / "Wake all" on the Fleet page and in Ctrl+K,
  and an "Asleep" (moon) pill. The simulator models all of the above
  (`SimConfig::doze_auto_after` shortens the 60 s for tests;
  `host/lumigift-core/tests/doze.rs`).

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
   only the `ceil(image_len / 2 KiB)` pages the image covers, in order
   (ahead of time when idle, or on demand when a chunk for an unerased page
   comes up, buffering chunks meanwhile). The host pauses for the typical
   erase time of those pages (22 ms each) so the stream meets erased flash.
3. `OtaChunk { session, index, data[24] }` for every chunk, in order, paced
   at the rate the gifts can program (see below).
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

The bottleneck is the gifts' flash and CPU, not the radio. Measured on a real
STM32C011 gift (datasheet: 85 µs per double word, 22 ms per 2 KiB page erase):

| | per chunk (24 B) | chunks/s |
|---|---:|---:|
| air, 1 Mbps NO_ACK run (28 B payload = 297 µs + run overhead) | ~360 µs | ~2,770 (bridge, measured) |
| gift, programming only: 3 double words + SPI drain + bookkeeping | ~450 µs | ~2,200 (measured) |
| gift, while also erasing (22 ms per 85 chunks) | ~710 µs | ~1,400 |

* **Erase first, then stream.** Streaming while the gifts still erased kept
  their flash ~90 % busy and lost ~3 % of chunks around every page erase on
  hardware, ~10 % at 1,700 chunks/s. So the host pauses for the erase
  (10 pages: 220 ms, done by all gifts in parallel) and then streams at
  90 % of the programming rate (`OtaOptions::double_word_time` = 140 µs
  effective, ≈2,000 chunks/s). Measured: ~1.5 % first-pass loss, one short
  repair round. A gift with slower flash (40 ms max) erases the page a chunk
  needs on demand, buffering the stream meanwhile.
* **Erase/program overlap reception.** The bootloader's erase/program
  sequences and the radio drain run from RAM (`.data.ramfunc.*`); while the
  flash is busy the CPU keeps moving packets from the 3-deep nRF RX FIFO into
  RAM rings (4 unicast + 80 broadcast slots, 2.8 KiB), and unicast queries
  are answered before buffered chunks.
* **2 Mbps: not used.** It would only shorten the air time, which is not the
  limit, while costing ~3 dB of sensitivity and adding a rate switch to the
  never-brick path.
* **Chunk size: kept at 24 bytes.** 28 data bytes per 32-byte packet would
  split double words across chunks, which out-of-order repair chunks make
  awkward to program exactly once.

On hardware, one gift and an 18.6 KB image: **0.70 s** (enter 25 ms, erase
220 ms, stream 395 ms, repair ~50 ms, reboot 10 ms), 10 of 10 consecutive
updates good. Simulated for 10 gifts with the 15.8 KB image and the measured
flash timing (`cargo test -p lumigift-core --test ota -- timing --nocapture`;
the simulator runs the real host code against modelled radio, bridge, USB,
flash, RX FIFO/rings and random loss):

| loss | 10 gifts |
|---:|---:|
| 0 % | 0.57 s |
| 5 % | 0.82 s |
| 20 % | 1.4 s |

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

## Lighting: generic looks and groups

Besides music shows, `lumigift-fx` has a library of generic looks: Solid
(constant colour), Breathe (fade in/out), Rainbow, Fade, Waterfall, Knight
Rider, Blink, Twinkle, Comet, Theater chase, Fire, Police, Timed strobe,
Timed pulse and Colour wipe (`lumigift_fx::EFFECTS` lists names, the
parameters that matter and defaults). They are rendered **on the gifts**
from the bridge-synced clock and the gift's slot, in integer math, so every
gift animates in lockstep with the preview and the radio only carries
parameters. They need no audio (all music fields 0); a few also react to
audio while a show plays (marked `Music::Optional`).

`MusicFrame` (32-byte broadcast, table in the `lumigift-proto` docs)
carries, besides the effect, palette and `param[0]` speed / `param[1]` size:

* `colors[2]` with `palette = PALETTE_CUSTOM` (0xFF): a primary → secondary
  gradient, hue 0 = exactly the primary (so Solid shows it unchanged).
* `mask: u32`: slot bitmask. A gift applies a frame only if its slot's bit is
  set (gifts without a slot only for `MASK_ALL`); other frames only keep it
  alive, so it keeps its previous look. `ShowOutput::Layers(Vec<MusicFrame>)`
  sends one frame per group every tick (up to 8, round-robin beyond), later
  layers winning on overlapping gifts. Spatial looks span the gifts in the
  mask, in slot order (all slots for `MASK_ALL`).
* `transition`: when the look (effect, palette, colours, params, hue,
  brightness) changes, the gift renders the old and the new look and blends
  them over `transition × 50 ms`; brightness 0 with a transition fades to
  black. Music shows send 0 (cuts, as before).

The gift applies the power limit, gamma and max-brightness cap after the
renderer as for every other frame.

**Studio: the Lighting page** (rail, between Studio and Stage, `Ctrl+3`)
shows every look as a card with a live preview (rendered with `lumigift-fx`
on a virtual row of gifts), a 3D stage preview, target (all gifts, the
selection, or named groups saved by gift UID), palette / custom colours,
speed, size, brightness, crossfade, master brightness, Fade out, Blackout and
saved favourites. Command palette: "Lighting: <effect>", favourites, Fade
out, Release.

**Output ownership.** Only one source drives the show stream at a time:

| Owner | How it gets the gifts | Status bar |
|---|---|---|
| Lighting page | applying a look (or "Take over"); switches the Studio's LIVE off | `● Lighting · Fade` |
| Studio show | LIVE on (`Ctrl+L`), or a show starting to play while Lighting is armed (it goes LIVE) | `● LIVE · 10 gifts` |
| nobody | releasing Lighting (restores LIVE if it was on before) or switching LIVE off | `○ Preview` |

The global blackout (`Ctrl+Shift+B`) overrides both.

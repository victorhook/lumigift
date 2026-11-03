# Lumigift gift firmware (STM32C011F6P6)

Radio bootloader + application for the Lumigift gift boxes, in Rust on the
bare `stm32-metapac` PAC. System design, pin table and the OTA flow are in
[`docs/ARCHITECTURE.md`](../../docs/ARCHITECTURE.md); the wire protocol and
flash layout live in [`crates/lumigift-proto`](../../crates/lumigift-proto).

```
firmware/lumigift/            own cargo workspace, default target thumbv6m-none-eabi
├── .cargo/config.toml        target, probe-rs runner, --nmagic, `cargo xtask` / `cargo test-host` aliases
├── rust-toolchain.toml       stable + thumbv6m-none-eabi
├── Cargo.toml                profiles: opt-level "z", fat LTO, 1 CGU, panic = abort, debug info
├── logic/                    lumigift-logic: pure, host-tested logic (bootloader recovery mode +
│                             OTA receiver, RX ring, channel plan, boot decision, WRP decision,
│                             flash write guard, config log, clock sync, seq stats, ADC maths,
│                             bootloader decoder). The host simulator runs this code too.
├── board/                    lumigift-board: clocks, GPIO, SPI1 + nRF24 bring-up, flash (guard,
│                             RAM-resident erase/program + radio drain, WRP), CRC, IWDG, µs timer,
│                             reset flags, BootShared, UID, config store
├── boot/                     lumigift-boot: radio bootloader, 0x0800_0000, ≤ 10 KiB
├── app/                      lumigift-app: the gift application, 0x0800_2900, ≤ 19.75 KiB
├── xtask/                    host tool (own workspace): OTA image, size report + RAM-code check,
│                             SWD flash, WRP removal
└── flash.sh                  one-time SWD flash (wraps `cargo xtask flash`)
```

## Build

```sh
cd firmware/lumigift
cargo build --release          # bootloader + app (dev builds use the same size settings)
cargo test-host                # unit tests of lumigift-logic on the host (42 tests)
cargo xtask size               # flash/RAM usage against the budgets, RAM-code check
cargo xtask image              # target/lumigift-app.img (+ .bin) for OTA
cargo xtask unprotect          # remove the bootloader's write protection over SWD
```

`cargo xtask image` builds the app, flattens the ELF's load segments into a
binary linked at `APP_VECTORS` (0x0800_2900), and prepends
`ImageHeader::new(app_bin, hw_rev = 1, fw_version, git_hash)` padded with
0xFF to 256 bytes. `fw_version` is the unix build time (override with
`--version N` or `SOURCE_DATE_EPOCH`), `git_hash` the first 8 hex characters
of `HEAD` as ASCII. The app reports its version by reading that header back
from flash, so the binary itself does not embed it.

> Run `cargo fmt -p <package>`, not `cargo fmt --all`: `--all` also
> reformats the shared path-dependency crates in `crates/`.

## Size report

`cargo xtask size` (release, rustc 1.98, `-inline-threshold=15`):

| Image | Flash | Budget | | Static RAM | RAM budget |
|---|---:|---:|---:|---:|---:|
| bootloader | 9 552 B | 10 224 B (10 KiB − 16 B info block) | 93.4 % | 3 376 B | 6 128 B |
| app | 18 160 B | 20 224 B (`APP_MAX_SIZE`) | 89.8 % | 244 B | 6 128 B |

RAM excludes the 16 bytes of `BootShared` at 0x2000_17F0 in both linker
scripts; everything not static is stack. Bootloader RAM: RX rings 2 796 B
(80 broadcast + 4 unicast slots of 33 B: enough for a worst-case 40 ms page
erase at the host's streaming rate), RAM functions 572 B (`ram_erase`,
`ram_program`, the radio `drain` and its SPI byte transfer; their flash copy
counts in the flash column), a few bytes of statics; ~2.7 KiB stack. The
app has the erase/program RAM functions (132 B) for its config writes.

The bootloader keeps ~670 B of headroom: it uses its own subset packet
decoder (the full `Packet::decode` is 1.6 KiB), size-optimised
`memcpy`/`memset`/division routines (`boot/src/tiny_builtins.rs`), one
`&dyn RegionRead` path for image validation, and LLVM's inliner at threshold
15 (`.cargo/config.toml`, ~300 B less than opt-level "z"'s default).

`cargo xtask size` also checks that the RAM functions (`.data.ramfunc.*`)
contain no `BL` to a flash address and no indirect call: they run while the
flash is busy, when any flash access would stall the CPU.

## First-time flash (SWD, ST-Link)

`cargo xtask flash` uses probe-rs (<https://probe.rs>) if it is installed,
otherwise STM32CubeProgrammer (`~/opt/STM32CubeProgrammer/bin` or on `PATH`):

```sh
./flash.sh                         # = cargo xtask flash
./flash.sh --dry-run               # only print the commands
cargo xtask flash --erase          # first flash of a gift with old firmware: mass erase first
cargo xtask flash --serial <SN>    # pick the ST-Link when several are connected (st-info --probe)
```

The ST-Link's reset line is not needed. With probe-rs this runs

```sh
probe-rs download --chip STM32C011F6Px target/thumbv6m-none-eabi/release/lumigift-boot
probe-rs download --chip STM32C011F6Px --binary-format bin --base-address 0x08002800 target/lumigift-app.img
probe-rs reset    --chip STM32C011F6Px
```

The bootloader ELF covers pages 0-4 (including the 16-byte info block at
0x0800_27F0 that tells the app the bootloader version), the image covers
the app region from 0x0800_2800. The config page at 0x0800_7800 is never
written by these commands, so a re-flash keeps a gift's node address and slot.

### Bootloader write protection

On its first boot the bootloader write-protects its own pages 0-4 (option
bytes, WRP area A: `FLASH_WRP1AR = 0x0004_0000`) and the option-byte reload
resets the chip once. From then on nothing, not even app code that bypasses
the flash driver, can erase or program the bootloader, and `Identity` /
`Status` report `BOOT_PROTECTED`. Flashing a new bootloader over SWD then
fails until the protection is removed (RDP must be level 0, the default):

```sh
cargo xtask unprotect   # = STM32_Programmer_CLI -c port=SWD [sn=<SN>] -ob WRP1A_STRT=0xF WRP1A_END=0x0
./flash.sh              # the new bootloader protects itself again on its first boot
```

(`WRP1A_STRT > WRP1A_END` disables the area; probe-rs cannot write the WRP
option bytes, STM32CubeProgrammer can. App images go to the app region and
are not affected.) If programming the option bytes ever fails, the
bootloader carries on unprotected; `BootShared.flags` limits the attempt to
once per power cycle, so it can never cause a reset loop.

Notes:

* `cargo run -p lumigift-boot --release` flashes and runs the bootloader alone
  (runner `probe-rs run`). Flashing the bare app ELF is not useful: without
  the image header the bootloader treats the region as empty and stays in
  recovery. Flash the image, then `probe-rs attach` to debug the app.
* The IWDG is frozen while a debugger halts the core (DBGMCU), so stepping
  does not reset the chip.
* Option bytes: factory default (boot from main flash, `nBOOT_SEL` etc.)
  works. If a chip was configured to boot from system memory, set
  `nBOOT_SEL = 1, nBOOT0 = 1` (PA14 is also BOOT0).

## Design notes

### HAL choice: bare PAC (`stm32-metapac` 21, feature `stm32c011f6`)

embassy-stm32 0.6 does support the C0, but the bootloader has to fit
10 KiB with an nRF24 driver, OTA receiver and flash driver, and the app
needs precise control of the ack-payload timing and of the WS2812 DMA. A
blocking super-loop over the PAC is small, has no executor/HAL runtime
overhead, and every register access is explicit and reviewable. The shared
`board` crate keeps both images on the same register code. `cortex-m-rt`
provides the vector tables; `stm32-metapac/rt` the device interrupts.

### Clocks and time

HSI48 undivided (HSIDIV = 1) as SYSCLK, AHB = APB = 48 MHz, one flash
wait state, prefetch + instruction cache on. TIM14 runs at 1 MHz; its 16-bit
counter is extended in software (`board::time`), folding a pending overflow
in on every read, so the bootloader can poll it and the app also takes the
TIM14 update interrupt.

### Radio

Both images bring the nRF24 up identically (`board::radio::start`): 1 Mbps,
2-byte CRC, 5-byte addresses, dynamic payloads, ack payloads, `EN_DYN_ACK`;
pipe 1 = `[0x00,"LGFT"]` broadcast, pipe 2 = `[node_addr,"LGFT"]` unicast,
pipe 0 off; only RX_DR asserts IRQ. Channel and node address come from the
config record (defaults from `ConfigRecord::defaults(uid)`). After every
unicast packet the firmware flushes the TX FIFO and writes exactly one ack
payload: the command's reply, else a `Status`. The app also refreshes the
preloaded `Status` every 100 ms while no other reply is pending. A health
check reads `CONFIG` every second and re-initialises a radio that browned
out or lost its configuration.

In the app the IRQ pin (PA4, pull-up) raises EXTI line 4 on the falling
edge; the handler only timestamps the edge. The main loop drains the FIFO
whenever the pin is low (and every 10 ms regardless): it clears RX_DR first,
then reads until empty, so a packet arriving meanwhile re-asserts the IRQ.

### Clock sync

Show frames carry the bridge µs time stamped right before transmission.
The sample pair is (stamp + air time, local time of the IRQ edge); only the
first payload after a fresh edge gets a sample, so main-loop latency never
enters the measurement. `lumigift_logic::sync::ClockSync` is a min-delay
envelope filter: per 8-frame window it keeps the least-delayed sample,
re-anchors the model on it and corrects the frequency (the HSI48 is only
±1-2 % accurate, 100-200 µs per frame). Simulated with ±2.5 % drift,
0-200 µs jitter and 3 ms stalls it stays within ~100 µs. `sync_err_us` is
the smoothed correction per anchor; `TIME_SYNCED` is set after 3 anchors
and cleared after 1 s without show frames. Effects render from
`synced_us / 1000`, i.e. from the bridge clock itself, so every gift
computes the same frame (the ms value wraps with the bridge's 32-bit µs
counter every 71.6 min, identically on all gifts).

### LEDs (WS2812B on PA2 = TIM1_CH3, AF5)

PWM + DMA, so the 800 kHz timing never depends on the CPU, which is
regularly stalled by flash operations and busy with SPI. TIM1 counts to 59
at 48 MHz (1.25 µs per bit); CCR3 = 17 (0.354 µs, "0") or 38 (0.792 µs,
"1"). DMA1 channel 1, requested by TIM1_UP via DMAMUX request 25, copies one
byte per bit into the CCR3 preload register (8-bit memory, 16-bit
peripheral). The 98-byte frame starts and ends with a 0 duty; the trailing
0 stays in CCR3 and holds the line low (latch/reset) until the next frame
10 ms later. A frame takes 122 µs.

Per frame (100 Hz): `Renderer::render` (Music) or the direct colours
(Solid/Leds/Blackout), an idle fade (show content fades out ~250 ms after
`idle_timeout` without show frames; the renderer additionally fades Music
by itself after 600 ms), then `power_limit(budget)`, `gamma`, and the
`max_brightness` cap, in that order. Because gamma and the cap only lower
values, the power budget still holds for what is sent. `Blink` overrides
the LEDs with its colour (count × period, on for the first half), then the
show resumes. A 0.7 s rainbow boot animation runs until the first show frame.

### OTA receiver (bootloader)

The bootloader's recovery mode is `lumigift_logic::recovery::Recovery`
(host-tested, also run by the host simulator); `boot/src/main.rs` is glue:

```text
loop: feed IWDG; drain radio → rings; match next_action():
        Unicast   → on_unicast()  → preload the reply as the next ack payload
        Broadcast → on_broadcast() (programs chunks, erasing pages on demand)
        Erase     → erase the next page of the session ahead of time
      tick(): channel switch / start the verified app / reboot
```

* **RX rings and RAM code.** Every erase/program runs its whole
  start-and-wait sequence from RAM (`board::flash::ram_erase`/`ram_program`,
  section `.data.ramfunc.*`, copied by cortex-m-rt's startup like any
  `.data`), and while the flash is busy it keeps calling the RAM-resident
  `board::fastrx::drain`, which reads packets from the nRF over raw SPI
  registers into two `RxRing`s (unicast pipe 2: 4 slots; broadcast pipe 1:
  80 slots). So reception continues through a 22-40 ms page erase. Unicast
  packets are handled first. A full ring drops packets (counted, reported as
  `Status.bcast_lost`).
* `OtaBegin` records the session (a repeated Begin of the running session is
  ignored, anything else restarts) and nothing else: the
  `ceil(image_len / 2048)` pages the image covers are erased strictly in
  order, page 0 (old header) first, either ahead of time while idle or on
  demand before a chunk for an unerased page is programmed.
* `OtaChunk` writes up to 3 double words at `index × 24`; a received-chunk
  bitmap (108 bytes) skips duplicates so every double word is programmed at
  most once; all-0xFF double words and bytes past the image are skipped. A
  flash error (read-back mismatch) fails the session.
* When the last chunk is stored the image is verified right away: header
  (magic, version, CRC, bounds, `hw_rev == 1`), vector table (SP in RAM
  below BootShared, Thumb reset vector inside the app), app CRC and the
  whole-image CRC announced in Begin → `Verified` or `Failed`.
* `OtaQuery { window }` returns an `OtaBitmap`: the first 192-chunk window at
  or after `window` with holes (chunks still in the ring count as
  received), and the total missing count. `OtaCommit` returns the same with
  the verdict.
* Replies: after a command the reply stays preloaded for up to 2 polls (a
  poll that arrived before the command was handled re-offers it), then
  polls get `Status` again.
* After `Verified` the app starts on the next `Poll` (its ack carried the
  verdict) or after 200 ms. Before the jump the bootloader stores the radio
  channel the update came on if the config says otherwise.

The image CRC uses the CRC unit (byte-reversed input, reversed output,
final XOR), self-tested once against the CRC-32 check value with a
software fallback.

### Boot flow

The bootloader latches `RCC_CSR2` into `BootShared` (watchdog > software >
low-power > POR/BOR > pin; a POR flag with `BootShared` intact in RAM is
reported as `Brownout`), clears the flags, re-initialises `BootShared` after
a power cycle and otherwise counts `resets`. It starts the IWDG (~2 s) in
every mode, then makes sure its pages are write-protected (see above). Boot
decision as documented: no valid image / `REQ_STAY_IN_BOOT` /
`boot_attempts ≥ 3` → recovery; otherwise 150 ms of listening for
`BootHold` **on the default channel**, then `boot_attempts += 1` and the
jump: radio powered down, GPIO/SPI1/TIM14/SYSCFG/CRC reset through RCC and
unclocked, SysTick off, NVIC cleared, VTOR = 0x0800_2900, MSP + reset vector
from the app's table. The app clears `boot_attempts` after 5 s, and starts
its radio before anything else (LEDs, ADC) so the host can confirm an update
within milliseconds.

Radio channel in recovery (`lumigift_logic::channel`): start on the
configured channel (or the default one after a `BootHold`), alternate with
the default channel every second while nothing is heard, stay on a channel
while any Lumigift packet arrives (until 3 s of silence). A missing or
corrupt config record means the defaults.

Recovery LED (PB6): double flash every 1.5 s idle, fast flicker while an
update runs, steady on after `Verified`, 1 Hz blink if the radio is missing.
`Reboot { into_bootloader: false }` from recovery also clears
`boot_attempts`, giving a crash-looping app a fresh set of tries.

### Flash write guard

`board::flash` refuses any erase/program below the image's write floor,
`__lumigift_flash_floor` from its `memory.x` (bootloader: 0x0800_2800, app
region + config page; app: 0x0800_7800, config page only), and never below
the app region whatever the symbol says (`lumigift_logic::boot::write_allowed`).

### Config log

Append-only 16-byte `ConfigRecord`s in the config page; the last valid
record wins, torn writes are skipped, a full page is erased and the record
rewritten at slot 0. Missing record → `ConfigRecord::defaults(uid)`.
`SetConfig` keys: `MAX_BRIGHTNESS` (0-255), `IDLE_TIMEOUT_MS` (stored in
100 ms units, 100-25 500 ms), `POWER_BUDGET_MA` (20-2000), `CHANNEL`
(0-125, switches 1 s after the reply). The reply carries the clamped value.

### VBAT and temperature

Every second: VREFINT (ADC_IN10) gives VDDA via `VREFINT_CAL`
(0x1FFF_756A, 30 °C / 3.0 V), VBAT = average of four ADC_IN3 conversions,
temperature from ADC_IN9 with `TS_CAL1` (0x1FFF_7568) and the typical
2.53 mV/°C slope. Calibration words that look unprogrammed fall back to
typical values. `LOW_BATTERY` below 1000 mV, cleared above 1050 mV
(`lumigift_logic::adc::LOW_BATTERY_MV`).

## Hardware bring-up checklist

Everything here is compiled and unit-tested only; no hardware was attached.

1. **SWD + clocks**: flash with `./flash.sh`, attach, check `RCC_CFGR.SWS = HSISYS`,
   `RCC_CR.HSIDIV = 0`, `FLASH_ACR.LATENCY = 1`; measure 48 MHz (e.g. MCO).
2. **BootShared**: power-cycle → `reset_reason = PowerOn`, `resets = 0`; NRST
   → `Pin`; `Reboot` → `Software`, `resets` increments. Check the RAM-parity
   option bit is off (reading uninitialised RAM must not fault).
3. **nRF24 on SPI1** (6 MHz, mode 0) with CSN on the "PA11 [PA9]" pad
   (`SYSCFG_CFGR1.PA11_RMP = 0`): `radio::start` must succeed (SETUP_AW
   readback), `CONFIG` reads 0x3F while running. Scope CSN/SCK.
4. **IRQ PA4**: falling edge per packet, EXTI4_15 fires, `EDGE_COUNT` increments.
5. **Unicast + ack payloads**: Poll → Status (`echo`, uptime), GetIdentity
   (UID, fw/boot version), SetSlot/SetConfig persist across reset.
6. **Discovery**: `SetNodeAddr` broadcast moves pipe 2 in app and bootloader;
   `EnterBootloader` with the gift's address reboots it into the bootloader.
7. **WS2812B**: scope PA2: 1.25 µs period, T0H ≈ 0.35 µs, T1H ≈ 0.79 µs,
   98 periods per frame then low; colours in GRB order; boot animation.
   Verify the DMAMUX request id 25 = TIM1_UP and that `BDTR.MOE` is set.
8. **Power**: full white must stay under `power_budget_ma`; watch the boost
   converter and the 3.3 V rail for brown-outs of the nRF (health check
   re-inits it).
9. **Clock sync**: two gifts, Music show: `TIME_SYNCED` set, `sync_err_us`
   ≲ 100, effects visibly in lockstep; `bcast_lost` matches link quality.
10. **ADC**: compare `vbat_mv` with a multimeter at 0.9/1.2/1.5 V;
    check `VREFINT_CAL`/`TS_CAL1` addresses and temperature plausibility
    (some C011 parts are reported to have an inaccurate `TS_CAL1`).
11. **IWDG**: hang the main loop (debugger off) → watchdog reset reported;
    three crashing boots → recovery mode (`Status` from the bootloader,
    `boot_attempts` 3 in `BootShared`).
12. **Write protection**: after the first boot (one extra reset, `resets`
    increments, reason `Software`), `STM32_Programmer_CLI -c port=SWD -ob displ`
    shows `WRP1A_STRT = 0x0`, `WRP1A_END = 0x4`; `Identity.flags` has
    `BOOT_PROTECTED`; a `probe-rs download` of the bootloader fails;
    `cargo xtask unprotect` removes it and the next boot re-applies it.
    Check that a power cycle after a (forced) failed attempt does not loop.
13. **RAM functions**: `ram_erase`/`ram_program`/`drain`/`xfer` sit at
    0x2000_0000.. (map file / `nm`); break in `ram_erase` and check FLASH_SR.BSY
    stays set while it loops, and that `drain` pulls packets meanwhile. Stream
    chunks at the host's automatic rate and at 3,000/s during the first page
    erases: at the automatic rate `Status.bcast_lost` (ring drops) stays 0.
14. **Flash timing**: measure a page erase (scope a GPIO toggled around
    `ram_erase`; 22 ms typ / 40 ms max) and a double word (85 µs); if they
    differ, set `OtaOptions::page_erase_time` / `double_word_time` (host's
    automatic rate) and `SimConfig::page_erase_time` accordingly. Worst case
    a chunk triggering several erases stays far below the 2 s IWDG.
15. **OTA end to end**: `lumigift ota target/lumigift-app.img` on 1 and on
    ~10 gifts; compare the phase times with the simulator's (≈0.5 s for 10
    gifts). Power-cut a gift mid-erase, mid-transfer and right after
    `Verified`: it must come back in recovery (or with the new image after
    `Verified`) and a re-run must succeed. Same image again → "already up to
    date".
16. **Channel fallback**: `lumigift config channel 90 --target <gift>` then
    `lumigift reboot <gift> --bootloader` while the host stays on 76: the
    bootloader alternates 90/76 every second and is found on 76 within ~1-2 s;
    after an OTA its config channel is 76 again.
17. **Rescue**: flash an app whose radio is disabled (or with a wrong
    channel), then `lumigift rescue` and power-cycle the gift: it shows up in
    its bootloader on channel 76 (the 150 ms window starts after the nRF's
    power-on delay, up to 100 ms), and `lumigift ota` fixes it.
18. **HW CRC**: confirm the self-test passes (otherwise the software CRC
    is used silently; only slower: ~12 ms instead of ~2.5 ms per 15 KB).
19. **TSSOP20 bonded pads**: keep PB3/PB4/PB5 (bonded with PB6) and PB8
    (bonded with PB7) in their reset analog state; confirm on the schematic.

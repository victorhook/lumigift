# nrf24-usb-bridge

Firmware for a generic nRF24L01+ ⇄ USB dongle built from an STM32F103C8 board and an
nRF24L01+PA+LNA module. It speaks only the protocol in
[`crates/nrf24-usb-proto`](../../crates/nrf24-usb-proto/src/lib.rs) and knows nothing
about any particular application (Lumigift uses it as its computer-side radio).

* Rust + [embassy](https://embassy.dev): embassy-stm32 0.6, embassy-executor 0.10,
  embassy-time 0.5 (1 MHz tick), embassy-usb 0.6, embassy-sync 0.8.
* USB full speed, vendor class 0xFF, bulk OUT `0x01` / bulk IN `0x81`, 64-byte packets,
  VID:PID `1209:0001`, serial number = the MCU's 96-bit UID in hex.
* MS OS 2.0 descriptors (WinUSB compatible ID and a DeviceInterfaceGUID
  `{9B5C2B1E-4D7A-4F0C-9C61-2A8E5F3D7B40}`), so Windows binds WinUSB without a driver.
* Release build: about 40 KiB flash (of the 48 KiB app region after the 16 KiB USB
  bootloader), 6.9 KiB static RAM (of 20 KiB). The
  rest of the RAM is stack.

## Hardware

### Board: "Tenstar Robot Mini STM32F103C8T6"

8 MHz crystal, user LED on PC13 (active low). Its USB-C data lines go to an onboard CH340
UART bridge, so native USB needs a small rewire:

| USB-C / cable | STM32 | Notes |
|---|---|---|
| D− | PA11 | cut or lift the CH340 D− connection, or use a separate USB connector |
| D+ | PA12 | same |
| D+ pull-up | PA12 → 1.5 kΩ → 3V3 | **required**: it tells the host a full-speed device is attached. The board has none, and without it the PC never sees the dongle (the F103 has no internal USB pull-up) |
| VBUS 5 V / GND | 5V / GND | as on the board |

The pull-up is fixed, so at boot the firmware drives PA12 low for 10 ms before it starts
the USB peripheral. The host sees a disconnect and enumerates the device again after
every reset or flash, the usual Blue Pill trick. (Many Blue Pill clones have a 10 kΩ or
4.7 kΩ resistor on PA12 instead of 1.5 kΩ. If enumeration is flaky, check that resistor.)

### Radio: nRF24L01+PA+LNA (SMA)

| nRF24 pin | STM32 | |
|---|---|---|
| VCC | 3V3 | **add 10–47 µF (low-ESR electrolytic or ceramic) right at the module's VCC/GND pins**, plus 100 nF |
| GND | GND | |
| SCK | PA5 | SPI1, 9 MHz (72 MHz / 8; the nRF maximum is 10 MHz) |
| MISO | PA6 | internal pull-down, so a missing module fails the probe cleanly |
| MOSI | PA7 | |
| CSN | PA4 | software chip select |
| CE | PB0 | |
| IRQ | PB1 | active low, EXTI1, internal pull-up |

The PA+LNA module draws current spikes of more than 100 mA when it transmits. The board's
3.3 V LDO and its thin traces cannot deliver those fast enough without the bulk capacitor.
The usual symptoms of a missing capacitor are a radio that "works at Power::Min but not at
Power::Max", random `MaxRetries`, or a radio that resets. If you can, power the module
from a separate 3.3 V regulator (for example an AMS1117 fed from 5 V). Keep the SMA antenna
attached whenever it transmits.

All pins are set in one place: [`src/board.rs`](src/board.rs) (the pin map at the top
of `Board::take`, plus the `EXTI1` binding, which must match the IRQ pin's number).

### LED

* Short blip once per second: firmware running.
* Fast flicker: radio traffic (TX loads, completions and received packets).

## Building

```sh
cd firmware/nrf24-usb-bridge
cargo build --release                  # the app (target from .cargo/config.toml)
(cd boot && cargo build --release)     # the USB bootloader (its own workspace)
```

## Flash layout and USB bootloader

| Range | Size | Content |
|---|---|---|
| `0x0800_0000` | 16 KiB | USB DFU bootloader ([`boot/`](boot/src/main.rs), about 11 KiB) |
| `0x0800_4000` | 48 KiB | the app (about 40 KiB) |

The bootloader never writes its own pages, so a bad or interrupted USB update
can only break the app, and the bootloader can always install a new one. The
app's first page (its vector table) is only committed after everything else is
written and verified, so an interrupted update leaves no valid app and the bridge
simply waits in DFU mode. It stays in the bootloader (LED double-blinks) when:

* the app was told to update (`DFU_DETACH` on its DFU runtime interface);
* there is no valid app;
* the reset button is pressed twice within about half a second.

In DFU mode the bridge enumerates as `1209:0002` ("nrf24-usb bootloader", same
serial number as the app) and speaks DFU 1.1 with ST's DfuSe extensions.

## Flashing

**Once, over SWD** (ST-Link on SWDIO/SWCLK/GND/3V3; NRST is not needed):

```sh
./flash-swd.sh        # builds both, writes bootloader + app (probe-rs, else OpenOCD)
```

**Every later update, over USB** (no ST-Link):

```sh
cargo build --release
cargo run --release -p lumigift-cli -- bridge-update \
    firmware/nrf24-usb-bridge/target/thumbv7m-none-eabi/release/nrf24-usb-bridge
```

This switches the running app into the bootloader, erases, writes, reads back,
and restarts into the new app (also works if the bridge already waits in DFU
mode). `dfu-util` works too, with a raw binary:

```sh
arm-none-eabi-objcopy -O binary target/thumbv7m-none-eabi/release/nrf24-usb-bridge app.bin
dfu-util -d 1209:0001,1209:0002 -a 0 -s 0x08004000:leave -D app.bin
```

Windows: the app's interfaces get WinUSB automatically, but the bootloader has no
MS OS descriptors, so DFU mode needs WinUSB bound once with Zadig.

The app's linker script starts at `0x0800_4000`, so the app can no longer run
without the bootloader. To go back to a bare app, flash both again with
`flash-swd.sh`.

## USB suspend

The bridge never enters USB suspend. On the F103 a spurious WKUP follows SUSP
within microseconds, which left embassy's USB stack "suspended" while the hardware
was awake; after a host autosuspend (TLP and similar on laptops) every open then
stalled for ~6 s until the host reset the device. A small handler that runs before
embassy's ([`UsbNeverSuspend`](src/board.rs)) clears SUSP/WKUP, so the device simply
stays awake (it is powered and its radio is on anyway). Opening an idle bridge
takes milliseconds.

## Diagnostics

Examples in `host/nrf24-usb` and `host/lumigift-core` that talk to the hardware
directly, below the link manager:

```sh
cargo run -p nrf24-usb --example bridge_info               # info, ping, stats
cargo run -p nrf24-usb --example radio_selftest            # wiring check, no other node needed
cargo run -p nrf24-usb --example raw_tx -- 604C474654 10 ack   # raw packets to an address
cargo run -p lumigift-core --example raw_exchange -- 60    # command/poll exchange with gift 0x60
```

`radio_selftest` checks every nRF pin: register access (CSN/SCK/MOSI/MISO), a
NO_ACK burst whose rate proves IRQ works (without it the bridge falls back to
polling and manages only ~600 packets/s instead of ~2,800), auto-retransmit to an
empty address (CE, MAX_RT), and a channel scan (the receiver).

## Host access

* **Linux**: install the udev rule from this directory:
  ```sh
  sudo cp 70-nrf24-usb.rules /etc/udev/rules.d/
  sudo udevadm control --reload-rules && sudo udevadm trigger
  ```
  Then replug the dongle. The rule tags the device `uaccess` (logged-in users) and sets
  mode 0660.
* **Windows** (8.1+): no driver or .inf needed. WinUSB binds automatically through the MS OS
  2.0 descriptors. If you plugged in an older firmware with the same VID:PID before,
  delete the device in Device Manager once, because Windows caches descriptors.
* **macOS**: works without a driver (IOKit / nusb).

## Protocol summary

The full contract is in `nrf24-usb-proto`. Each USB packet carries one or more complete
`[len][kind][body]` messages, and `len == 0` ends a packet early.

| Host → bridge | Reply |
|---|---|
| `GetInfo` | `Info` (protocol version, fw version, `tx_queue_depth = 24`, `radio_present`, UID). Retries the radio probe if no radio was found. |
| `Ping{seq}` | `Pong{seq, time_us}` |
| `Configure(RadioConfig)` | `CmdResult` (`BadArgs` if channel > 125, crc_len > 2, addr_width ∉ 3..=5, retries/retry_delay > 15, unknown feature bits, payload_len ∉ 1..=32) |
| `SetRxPipe{pipe 0..=5, …}` | `CmdResult` |
| `SetTxAddr{slot 0..=7, addr}` | `CmdResult` (bridge-side storage; `Ok` even without a radio) |
| `Tx{tag, slot, flags, ts_offset, payload}` | exactly one `TxDone{tag, status, retries, time_us, ack_payload}` |
| `SetAckPayload{pipe, payload}` | `CmdResult` (`BadArgs` without the ACK_PAYLOAD feature or with an empty payload; `RadioError` if the TX FIFO is full) |
| `SetMode(PowerDown/Standby/Rx)` | `CmdResult` |
| `Scan{first, last, dwell_us, repeats}` | one `ScanResult{channel, hits, samples = repeats}` per channel |
| `GetStats` | `Stats` |
| `Reset` | `CmdResult Ok`, then a system reset (and re-enumeration) about 5–50 ms later |

Details the proto docs leave open, which the host driver should know:

* **Ordering**: the bridge executes messages strictly in order, one at a time. Replies come
  back in the same order.
* **Tx status**: `Sent` = sent with NO_ACK. `Acked` carries `retries` = ARC_CNT from
  OBSERVE_TX and the ack payload, if one arrived. `MaxRetries` has `retries` = the configured
  count. `Failed` means one of: slot ≥ 8, empty payload, payload longer than `payload_len`
  in static-payload mode, `STAMP` with `ts_offset + 4 > len`, `NO_ACK` without the `DYN_ACK`
  feature, no radio, or an SPI/radio fault. A malformed `Tx` message whose tag can still be
  read also gets `TxDone Failed`, so host flow control never leaks a slot. `time_us` is the
  time of completion.
* **Static payloads** (DYN_PAYLOAD and ACK_PAYLOAD both off): a shorter Tx payload is
  zero-padded to `payload_len`.
* **STAMP**: the u32 LE µs timestamp is written into the payload just before the
  `W_TX_PAYLOAD` SPI transfer. A stamped packet only goes into an empty TX FIFO, so the
  delay from stamp to air is always the same: about 130 µs PLL settle + SPI, plus the
  packet's own airtime.
* **Pipe 0**: an auto-ack Tx borrows RX pipe 0 (its address is set to the destination, with
  EN_RXADDR/EN_AA bit 0 on). The configured pipe 0 address is written back before the
  radio listens again. In Standby mode nothing listens, so it is restored lazily.
* **Rx mode**: the radio is a PRX whenever no command is being executed, and it raises an
  `Rx` event for each received packet. `time_us` is when the IRQ was serviced. Packets still
  in the RX FIFO when a Tx begins are reported first.
* **Ack payloads share the nRF's TX FIFO.** Any `Tx` flushes ack payloads that are still
  queued, because they would otherwise go out as data packets. Queue them again after
  transmitting if you need them.
* **Tx in PowerDown mode** powers the radio up (1.5 ms), transmits, and powers it down
  again.
* **Scan**: for every sample the bridge enters RX, waits 130 µs + max(dwell_us, 40) µs,
  reads RPD (> −64 dBm), and leaves RX. If the arguments are bad (first > last, last > 125,
  repeats == 0), or if the radio is missing or fails during the scan, the bridge replies
  `CmdResult{kind: Scan, BadArgs | RadioError}` instead of (or after some of) the
  ScanResults. Afterwards the radio returns to the configured channel and mode.
* **Event loss**: only unsolicited `Rx` events are ever dropped. They are counted in
  `Stats.events_dropped` once the IN queue is more than 36 of 48 entries full. Every
  reply (`TxDone`, `CmdResult`, `Pong`, `Info`, `Stats`, `ScanResult`) waits for space
  instead. A host that sends commands must therefore keep reading IN.
* **Malformed messages** count in `Stats.bad_commands`. Kinds that normally answer with
  `CmdResult` still answer `BadArgs`. A broken frame drops the rest of its USB packet.
* **IN transfers**: a 64-byte IN packet is followed by a zero-length packet when nothing
  else is queued. Hosts can therefore submit reads of 64 bytes or larger. A ZLP parses as
  zero messages.
* **Reconfiguration**: when the host (re)configures the device, events left over from a
  previous session are discarded.

## Performance design

```
 bulk OUT ─► out_task ─► CMDS (24 packets) ─► radio_task ─► EVENTS (48) ─► in_task ─► bulk IN
                                               │  ▲
                                         SPI 9 MHz │ IRQ (EXTI1)
                                               ▼  │
                                              nRF24L01+
```

* **out_task** copies each 64-byte OUT packet into the `CMDS` channel. When the channel is
  full it stops reading, so the endpoint NAKs. That is USB-level backpressure and nothing
  is lost. The radio task never waits on USB to receive commands.
* **radio_task** decodes one message at a time with one message of lookahead, executes it,
  and sleeps on the nRF IRQ pin (`ExtiInput::wait_for_low`) instead of polling. It waits for
  the *level*, not the falling edge: the nRF holds IRQ low until its flags are cleared, so
  a level wait cannot miss an edge that arrived while the task was busy. A 2–5 ms timer
  runs beside every IRQ wait. If the IRQ wire is broken, the firmware still works by
  polling STATUS, only slower.
* **in_task** blocks for the first event, then packs every other queued event into the
  same 64-byte packet (`PacketBuilder`) and sends it at once. While that packet waits for
  the host's IN token, more events build up and go out together in the next one. A
  `TxDone` without an ack payload is 10 bytes, so up to 6 fit in one packet.
* **Flow control**: `Info.tx_queue_depth = 24` = the `CMDS` capacity. Each queued packet
  holds at least one message, so a host that keeps at most 24 Tx in flight never blocks on
  an OUT write. The 48-entry event queue always has room for the matching TxDones, so
  host writes and reads cannot deadlock.

### NO_ACK streaming

A `Tx` with `NO_ACK` starts a burst. CE stays high, and the radio task keeps the 3-deep TX
FIFO topped up from the command queue for as long as the next queued message is also a
valid NO_ACK `Tx` to the same slot. The packets go out back to back, with TX_DS raised per
packet.

* **Correct completion accounting.** TX_DS flags coalesce, so counting IRQs would be wrong.
  The bridge clears STATUS first and then reads FIFO_STATUS. EMPTY means every loaded
  packet is done, FULL means 3 are left, and anything else means at most min(n, 2) are
  left. TxDones therefore come out in order and never early. At worst they are one packet
  time late at the tail of a burst. The bridge never loads more packets than this count
  allows, and a load that hits a full FIFO (STATUS.TX_FULL) counts as a fault, never as
  `Sent`.
* **4 ms rule.** The nRF must not stay in TX mode for more than 4 ms at a time, because its
  PLL runs open loop. Each continuous run is capped at `3500 µs / airtime(32 B)` packets
  (2 Mbps: 21, 1 Mbps: 10, 250 kbps: 2). After that the FIFO drains to empty
  (standby-II), and the next load re-settles the PLL (130 µs).
* A stamped packet only enters an empty FIFO (see STAMP above). An acked Tx, a different
  slot, or any other command ends the burst. The bridge then processes that message in
  order.

### Estimated throughput (32-byte payload, 5-byte address, 2-byte CRC)

Airtime is 8·(1 + 5 + 32 + 2) + 9 = 329 bits, which takes 164.5 µs at 2 Mbps.

| Mode | Estimate |
|---|---|
| NO_ACK, 2 Mbps | One run is 130 µs settle + 21 × 164.5 µs + about 60 µs refill, for 21 packets. That gives **about 5,700 packets/s, about 180 kB/s of payload**. If the chip re-settles for 130 µs between FIFO packets anyway (to be measured), expect about 3,300 packets/s. |
| NO_ACK, 1 Mbps | about 2,900 packets/s |
| NO_ACK, 250 kbps | about 700 packets/s |
| Auto-ack, 2 Mbps | One at a time: SPI + 130 µs settle + packet + 130 µs turnaround + ACK + IRQ service, about 550 µs, so **about 1,800 Tx/s** (fewer with ack payloads or retries) |

MCU budget per streamed packet: one IRQ wakeup, then about 37 bytes over SPI (clear
STATUS, read FIFO_STATUS, load 33 bytes) at 9 MHz plus decode. That is about 50–60 µs, well
under the 164 µs airtime. USB needs one 64-byte OUT packet per 32-byte Tx (two Tx with
payloads up to 18 B share a packet), so about 5,700 OUT packets/s. That is far below what
full-speed bulk can carry, even with the F1's single-buffered endpoints. The 24-deep window
holds about 4 ms of air at 2 Mbps, which covers the host's USB scheduling latency.

## Bring-up checklist / test procedure

1. **Before you power it**: check the D+ (PA12) 1.5 kΩ pull-up to 3V3, check that D−/D+ are
   not swapped, and check the 10–47 µF capacitor at the nRF module.
2. **Flash** with an ST-Link (see above). The LED should blip once per second.
3. **Enumeration**: `lsusb -d 1209:0001` should show `nrf24-usb bridge`.
   `lsusb -v -d 1209:0001` should show one interface (class 255) with EP 0x01 OUT and EP
   0x81 IN, bulk, 64 bytes. `dmesg -w` while you replug should show no errors. After a
   reset (`st-flash reset`) the device must enumerate again by itself (the D+ trick).
4. **Windows** (optional): in Device Manager the device should appear under "Universal
   Serial Bus devices" with the WinUSB driver.
5. **Protocol smoke test** with pyusb (`pip install pyusb`), after installing the udev rule:
   ```python
   import usb.core, struct
   d = usb.core.find(idVendor=0x1209, idProduct=0x0001); d.set_configuration()
   def cmd(*msgs): d.write(0x01, b"".join(bytes([len(m)]) + m for m in msgs))
   def read(): return bytes(d.read(0x81, 64, timeout=1000))
   cmd(b"\x01")                         # GetInfo
   print(read().hex())                  # 12 81 01 0100 18 01 <uid>  (radio_present=01)
   cmd(b"\x02" + struct.pack("<I", 7))  # Ping
   print(read().hex())                  # 09 82 07000000 <time_us>
   # SetTxAddr slot 0 = E7E7E7E7E7, then a NO_ACK Tx (tag 1) of 8 bytes
   cmd(b"\x05\x00" + b"\xe7" * 5, b"\x06" + struct.pack("<HBBB", 1, 0, 1, 0) + bytes(8))
   print(read().hex())                  # 03 87 05 00 (CmdResult Ok), then 09 84 0100 00 00 <t> (TxDone Sent)
   # (the two replies may arrive in one IN packet or two; read again if needed)
   cmd(b"\x09" + bytes([0, 125]) + struct.pack("<HB", 200, 20))  # Scan 0..125
   ```
   With no radio wired, `radio_present` is `00` and the Tx returns status `03` (Failed).
6. **Radio link**: use a second nRF24 node (for example a Lumigift gift, or an Arduino
   running RF24) as a PRX on channel 76, 1 Mbps, with auto-ack on the same address. An acked
   Tx should return `Acked` (and the ack payload, if the node preloads one). Power the
   receiver off and the Tx should return `MaxRetries` with `retries = 3`.
7. **Rx**: `SetRxPipe{1, enabled, auto_ack, addr}`, then `SetMode(Rx)`. Packets from the
   other node should show up as `Rx` events, and the LED should flicker.
8. **Scan**: with Wi-Fi traffic nearby, channels 1–60 (2401–2460 MHz) should show
   noticeably more hits than channels above 85.
9. **Throughput**: send about 10 000 NO_ACK Tx with up to 24 in flight at 2 Mbps. All
   should come back `Sent`, `GetStats` should show `tx_sent` matching, and you can compute
   packets/s from the `TxDone.time_us` values. Compare with the estimate above.
10. **Supply check**: repeat steps 6 and 9 at `Power::Max`. If you get more failures than at
    `Power::Min`, the module's 3.3 V supply is sagging (add capacitance or a separate LDO).

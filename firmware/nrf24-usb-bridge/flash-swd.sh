#!/usr/bin/env bash
# One-time (or recovery) flash over SWD with an ST-Link: the USB bootloader at
# 0x08000000 and the app at 0x08004000. After this, update over USB:
#   cargo run --release -p lumigift-cli -- bridge-update \
#       firmware/nrf24-usb-bridge/target/thumbv7m-none-eabi/release/nrf24-usb-bridge
#
# Uses probe-rs if installed, otherwise OpenOCD. `CPUTAPID 0` accepts the
# Cortex-M3 ID of clone chips (CKS32, APM32, ...).
set -euo pipefail
cd "$(dirname "$0")"

(cd boot && cargo build --release)
cargo build --release

boot=boot/target/thumbv7m-none-eabi/release/nrf24-usb-boot
app=target/thumbv7m-none-eabi/release/nrf24-usb-bridge

if command -v probe-rs >/dev/null; then
    probe-rs download --chip STM32F103C8 "$boot"
    probe-rs download --chip STM32F103C8 "$app"
    probe-rs reset --chip STM32F103C8
else
    openocd -f interface/stlink.cfg -c "set CPUTAPID 0" -f target/stm32f1x.cfg \
        -c "program $boot verify" -c "program $app verify reset exit"
fi
echo "done: the LED blips once per second when the app runs"

#!/usr/bin/env sh
# SWD flash of a Lumigift gift with an ST-Link (probe-rs, or STM32CubeProgrammer
# if probe-rs is not installed): bootloader ELF (0x0800_0000) + OTA app image
# (0x0800_2800), then reset. The config page (0x0800_7800) is not touched.
# For a new board or a full re-flash use ./flash_new.sh instead.
#
#   ./flash.sh                 build + flash
#   ./flash.sh --dry-run       print the commands only
#   ./flash.sh --serial SN     pick one of several ST-Links (CubeProgrammer)
set -eu
cd "$(dirname "$0")"
exec cargo xtask flash "$@"

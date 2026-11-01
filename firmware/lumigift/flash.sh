#!/usr/bin/env sh
# One-time SWD flash of a Lumigift gift with an ST-Link through probe-rs:
# bootloader ELF (0x0800_0000) + OTA app image (0x0800_2000), then reset.
# The config page (0x0800_7800) is not touched.
#
#   ./flash.sh                 build + flash
#   ./flash.sh --dry-run       print the probe-rs commands only
#   ./flash.sh --probe 0483:3748[:SERIAL]   pick one of several probes
set -eu
cd "$(dirname "$0")"
exec cargo xtask flash "$@"

#!/usr/bin/env sh
# Flash a gift from scratch over SWD with the (single) connected ST-Link:
# new boards with factory firmware, or gifts that already run ours.
#
#   1. remove the bootloader's write protection (no-op on a fresh chip)
#   2. mass-erase, write bootloader + app image, reset
#
# The erase also clears the config page: the gift starts with the node
# address derived from its UID and no slot (assign with `lumigift auto-slots`).
#
#   ./flash_new.sh              build + flash
#   ./flash_new.sh --dry-run    print the commands only
#   ./flash_new.sh --serial SN  pick one of several ST-Links (st-info --probe)
set -eu
cd "$(dirname "$0")"
cargo xtask unprotect "$@"
cargo xtask flash --erase "$@"
echo
echo "Flashed. With the bridge plugged in:"
echo "  cargo run --release -p lumigift-cli -- scan"
echo "  cargo run --release -p lumigift-cli -- auto-slots"

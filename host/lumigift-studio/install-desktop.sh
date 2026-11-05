#!/usr/bin/env bash
# Install Lumigift Studio as a desktop app for the current user (Ubuntu/GNOME
# and other freedesktop desktops): release build, binary in ~/.local/bin, icon
# and launcher (with a "Start with simulator" action). Re-run after updating
# the code to install the new build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
bin_dir="$HOME/.local/bin"
apps_dir="$HOME/.local/share/applications"
icon_dir="$HOME/.local/share/icons/hicolor/scalable/apps"

echo "Building Lumigift Studio (release)…"
cargo build --release -p lumigift-studio --manifest-path "$root/Cargo.toml"

install -Dm755 -s "$root/target/release/lumigift-studio" "$bin_dir/lumigift-studio"
install -Dm644 "$root/host/lumigift-studio/assets/lumigift-studio.svg" "$icon_dir/lumigift-studio.svg"
mkdir -p "$apps_dir"
cat > "$apps_dir/lumigift-studio.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Lumigift Studio
Comment=Light shows and looks for the Lumigift gift boxes
Exec=$bin_dir/lumigift-studio
Icon=lumigift-studio
Terminal=false
Categories=AudioVideo;Audio;
Keywords=lumigift;lights;led;show;
StartupWMClass=lumigift-studio
Actions=simulator;

[Desktop Action simulator]
Name=Start with simulator
Exec=$bin_dir/lumigift-studio --sim 10
EOF

update-desktop-database "$apps_dir" 2>/dev/null || true
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

echo "Installed: $bin_dir/lumigift-studio"
echo "Launcher:  $apps_dir/lumigift-studio.desktop"
echo "Find \"Lumigift Studio\" in the app grid (right-click it to pin it to the dock)."

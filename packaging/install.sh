#!/usr/bin/env bash
# Build and install FQShare for the current user (no root needed).
# Optional, once, for fast Wi-Fi transfers:  sudo bash packaging/fqshare-hotspot-setup.sh
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release
install -Dm755 target/release/fqshare "$HOME/.local/bin/fqshare"
install -Dm644 packaging/fqshare.service "$HOME/.config/systemd/user/fqshare.service"
install -Dm644 packaging/fqshare.desktop "$HOME/.local/share/applications/fqshare.desktop"
install -Dm644 README.md "$HOME/.local/share/doc/fqshare/README.md"
mkdir -p "$HOME/Downloads"

systemctl --user daemon-reload
systemctl --user enable --now fqshare.service
systemctl --user --no-pager status fqshare.service | head -5
echo "Installed. Click the phone icon in the tray to receive."

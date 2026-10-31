#!/usr/bin/env bash
# Remove FQShare for the current user. Received files are kept.
set -u
systemctl --user disable --now fqshare.service 2>/dev/null
rm -f "$HOME/.local/bin/fqshare" \
      "$HOME/.config/systemd/user/fqshare.service" \
      "$HOME/.local/share/applications/fqshare.desktop" \
      "$HOME/.local/share/applications/fqshare-tui.desktop"
rm -rf "$HOME/.local/share/doc/fqshare"
systemctl --user daemon-reload
echo "Removed. Config stays in ~/.config/fqshare; firewall zone: sudo bash packaging/fqshare-hotspot-setup.sh --remove"

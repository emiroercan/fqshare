#!/usr/bin/env bash
# One-time setup for FQShare transfers over a laptop-hosted hotspot.
#
# Creates the firewalld zone "fqshare". The temporary hotspot interface is
# put in this zone, which only lets the phone get an address (DHCP, DNS) and
# reach the transfer ports. Everything else on the laptop stays closed to it.
# Without this zone the app refuses to start a hotspot and keeps transfers on
# Bluetooth.
#
# Run:   sudo bash fqshare-hotspot-setup.sh
# Undo:  sudo bash fqshare-hotspot-setup.sh --remove
set -euo pipefail

ZONE=fqshare
PORTS=47900-47909/tcp   # keep in sync with HOTSPOT_PORTS in crates/rqs_lib/src/hdl/hotspot.rs

if [[ $EUID -ne 0 ]]; then
    echo "Run with sudo." >&2
    exit 1
fi

if [[ ${1:-} == --remove ]]; then
    if firewall-cmd --permanent --get-zones | tr ' ' '\n' | grep -qx "$ZONE"; then
        firewall-cmd --permanent --delete-zone="$ZONE"
        firewall-cmd --reload
        echo "Removed zone $ZONE."
    else
        echo "Zone $ZONE doesn't exist."
    fi
    exit 0
fi

if ! firewall-cmd --permanent --get-zones | tr ' ' '\n' | grep -qx "$ZONE"; then
    firewall-cmd --permanent --new-zone="$ZONE"
fi
firewall-cmd --permanent --zone="$ZONE" --set-short="FQShare hotspot"
firewall-cmd --permanent --zone="$ZONE" \
    --set-description="Temporary FQShare hotspot: DHCP, DNS and the transfer ports only."
firewall-cmd --permanent --zone="$ZONE" --set-target=default   # reject everything else
firewall-cmd --permanent --zone="$ZONE" --add-service=dhcp
firewall-cmd --permanent --zone="$ZONE" --add-service=dns
firewall-cmd --permanent --zone="$ZONE" --add-port="$PORTS"
firewall-cmd --reload

echo "Zone $ZONE ready:"
firewall-cmd --zone="$ZONE" --list-all

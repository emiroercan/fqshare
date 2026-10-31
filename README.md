# Fedora Quick Share

Receive Quick Share (Nearby Share) transfers from an Android phone on Linux,
**without a shared network**. The phone can be on mobile data and the laptop on
any Wi-Fi (or none): nothing goes through a router or the internet.

Tested with a Galaxy S22 (Android 14, Google Play services 26.42) and Fedora 44
(KDE Plasma, NetworkManager, BlueZ 5.87, Realtek RTL8852BE).

## How it works

1. **Discovery – Bluetooth LE.** While receiving, the laptop broadcasts a Nearby
   advertisement (service 0xFEF3). The phone lists it in Quick Share.
2. **Handshake – Bluetooth LE GATT.** The phone connects over BLE and runs the
   normal encrypted Quick Share handshake (UKEY2). No pairing.
3. **Consent.** A notification shows who is sending, what, and the PIN, with
   Accept / Decline. Nothing is saved before you accept.
4. **Data.**
   - Small transfers (< 1 MB: texts, links, small files) stay on Bluetooth.
   - Bigger ones move to Wi-Fi, still end-to-end encrypted:
     - if the phone is on the same network as the laptop, over that network;
     - otherwise (or if the network isolates devices, as public Wi-Fi often
       does) the laptop hosts a short-lived WPA2 hotspot, gives the phone its
       credentials over the encrypted channel, and the phone joins it.

   With a single Wi-Fi card the hotspot takes over the card, so **the laptop's
   own Wi-Fi drops for the few seconds of the transfer** and reconnects
   afterwards. (The card can do both at once, but NetworkManager removes any
   Wi-Fi Direct group it didn't create itself, so the Windows-style "hotspot
   while connected" isn't possible without bypassing it.)

## Install

```sh
packaging/install.sh                        # builds, installs for your user, starts the service
sudo bash packaging/fqshare-hotspot-setup.sh  # once, for fast Wi-Fi transfers
```

Then click the phone icon in the system tray: the laptop is visible to phones
for 5 minutes and hides again. On the phone: Share → Quick Share → your laptop.

Without the hotspot setup FQShare still works, but large transfers stay on
Bluetooth (roughly 20 KB/s) unless the phone is on the laptop's network.

Config (optional), `~/.config/fqshare/config.toml`:

```toml
device_name = "My laptop"            # default: hostname
download_dir = "/home/me/Downloads"  # default: XDG Downloads (see Sandbox below)
visible_minutes = 5
```

Uninstall: `packaging/uninstall.sh` (and `sudo bash packaging/fqshare-hotspot-setup.sh --remove`).

## Security model

Built for use on public networks and around untrusted devices.

- **Hidden by default.** Nothing is advertised until you click *Receive*; it
  hides again after `visible_minutes` (never mid-transfer). While hidden the
  Bluetooth advertisement and GATT service are off.
- **Nothing on the network.** No mDNS announcement and no listening port on
  the LAN. A Wi-Fi port is opened only after you accept a transfer, only on
  the address the phone was told, only for that transfer, and it accepts only
  the phone that started it (endpoint id check).
- **Every transfer needs your Accept**, with the sender name and PIN shown.
  Unanswered or dismissed requests are declined after 60 s.
- **Bluetooth sessions are pinned** to the device that opened them; packets
  from other nearby devices are dropped.
- **Hotspot:** random SSID and password per transfer, WPA2-CCMP, never saved
  (volatile NetworkManager profile, bound to the app so it disappears if the
  app dies), removed right after the transfer. Its interface sits in the
  `fqshare` firewalld zone, which allows only DHCP, DNS and TCP 47900–47909.
  If the zone doesn't exist FQShare refuses to start a hotspot.
- **Received files:** names are reduced to a plain file name (no `../` or
  absolute paths), existing files are never overwritten, files are created
  mode 0600.
- **Texts and links** are shown escaped and never acted on automatically.
  Links open only on click, only http(s), through the desktop portal.
  Wi-Fi credentials shared from the phone are shown, not joined.
- **Sandbox:** the systemd user service runs with a read-only system and home
  (write access only to `~/Downloads`), no capabilities, a system-call filter,
  and only Unix, IP, netlink and Bluetooth sockets. If you change
  `download_dir`, allow it with `systemctl --user edit fqshare`:

  ```ini
  [Service]
  ReadWritePaths=/path/to/your/dir
  ```

## Not supported (yet)

- Sending from the laptop to the phone.
- Concurrent transfers from several phones (one Bluetooth session at a time).
- Google-account "contacts only" visibility: while *Receive* is on, the laptop
  is visible to everyone nearby (every transfer still needs your Accept).

## Credits and licence

GPL-3.0-only. The Quick Share protocol library in `crates/rqs_lib` comes from
[rquickshare](https://github.com/Martichou/rquickshare) (Martichou) via
[nozwock's fork](https://github.com/nozwock/rquickshare) used by Packet, with
the BLE receiver and Wi-Fi upgrade from
[Martin Alderson's branch](https://github.com/martinalderson/rquickshare/tree/feat/ble-receiver-connect-back)
(see `docs/BLE_RECEIVER_DISCOVERY.md` there). Protocol details were checked
against [google/nearby](https://github.com/google/nearby). FQShare adds the
hotspot upgrade, medium fallback, session hardening and the desktop app.

//! Short-lived Wi-Fi hotspot for the WIFI_HOTSPOT bandwidth upgrade.
//!
//! When the phone isn't on our network, the receiver hosts a WPA2 access point
//! through NetworkManager and hands the phone its credentials over the already
//! encrypted Nearby channel. The phone joins, opens TCP to us, and the transfer
//! continues over Wi-Fi.
//!
//! Safety properties:
//! - random SSID and password for every hotspot;
//! - the NM profile is volatile (never written to disk) and bound to our D-Bus
//!   connection, so NM tears it down if this process dies;
//! - the interface is put in a dedicated firewalld zone that must already exist
//!   (created once by `packaging/fqshare-hotspot-setup.sh`). If it doesn't
//!   exist we refuse to start rather than fall back to a permissive zone.
//!
//! If the Wi-Fi card is busy with a normal connection and there's no spare
//! AP-capable interface, the hotspot takes over the card for the transfer and
//! the previous connection is reactivated afterwards.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::blocking::{Connection, Proxy};
use dbus::Path;
use rand::Rng;
use rand::distr::Alphanumeric;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const NM_WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const NM_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_IP4: &str = "org.freedesktop.NetworkManager.IP4Config";
const TIMEOUT: Duration = Duration::from_secs(10);

const NM_DEVICE_TYPE_WIFI: u32 = 2;
const NM_WIFI_DEVICE_CAP_AP: u32 = 0x40;
const NM_DEVICE_STATE_DISCONNECTED: u32 = 30;
const NM_DEVICE_STATE_ACTIVATED: u32 = 100;
const NM_ACTIVE_STATE_ACTIVATED: u32 = 2;
const NM_ACTIVE_STATE_DEACTIVATED: u32 = 4;

/// Firewalld zone the hotspot interface is placed in.
pub const HOTSPOT_ZONE: &str = "fqshare";
static ZONE_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Testing only: use another (existing) firewalld zone instead of
/// `HOTSPOT_ZONE`, e.g. before the one-time setup has been run.
pub fn set_hotspot_zone_for_testing(zone: &str) {
    let _ = ZONE_OVERRIDE.set(zone.to_string());
    warn!("Hotspot: TESTING zone override -> '{zone}'");
}

/// Testing only: an access point managed outside this process
/// ("ssid|password|gateway|frequency"), used instead of creating one via NM.
static EXTERNAL: std::sync::OnceLock<(String, String, Ipv4Addr, i32)> = std::sync::OnceLock::new();

pub fn set_external_hotspot_for_testing(spec: &str) -> Result<(), anyhow::Error> {
    let parts: Vec<&str> = spec.split('|').collect();
    let [ssid, password, gateway, freq] = parts[..] else {
        return Err(anyhow!("expected ssid|password|gateway|frequency"));
    };
    let _ = EXTERNAL.set((ssid.into(), password.into(), gateway.parse()?, freq.parse()?));
    warn!("Hotspot: TESTING external access point '{ssid}' at {gateway}");
    Ok(())
}

/// Whether the one-time setup (firewalld zone) has been done.
pub async fn hotspot_setup_done() -> bool {
    tokio::task::spawn_blocking(|| zone_exists(zone())).await.unwrap_or(false)
}

fn zone() -> &'static str {
    ZONE_OVERRIDE.get().map(String::as_str).unwrap_or(HOTSPOT_ZONE)
}
/// TCP ports the zone opens; the upgrade listener binds one of these.
pub const HOTSPOT_PORTS: std::ops::RangeInclusive<u16> = 47900..=47909;

/// Band/channel attempts, fastest first. 5 GHz channel 36 is non-DFS indoors
/// in most regulatory domains; 2.4 GHz channel 6 works everywhere.
const BANDS: [(&str, u32, i32); 2] = [("a", 36, 5180), ("bg", 6, 2437)];

pub struct Hotspot {
    conn: Mutex<Connection>,
    active: Path<'static>,
    /// Connection that was on the device before we took it over.
    restore: Option<(Path<'static>, Path<'static>)>,
    pub ssid: String,
    pub password: String,
    pub gateway: Ipv4Addr,
    pub frequency: i32,
    pub interface: String,
}

impl std::fmt::Debug for Hotspot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the password.
        f.debug_struct("Hotspot")
            .field("ssid", &self.ssid)
            .field("interface", &self.interface)
            .field("gateway", &self.gateway)
            .field("frequency", &self.frequency)
            .finish_non_exhaustive()
    }
}

impl Hotspot {
    /// Bring up a hotspot. Blocking D-Bus work runs off the async runtime.
    pub async fn start() -> Result<Self, anyhow::Error> {
        tokio::task::spawn_blocking(Self::start_blocking).await?
    }

    fn start_blocking() -> Result<Self, anyhow::Error> {
        if let Some((ssid, password, gateway, frequency)) = EXTERNAL.get() {
            return Ok(Self {
                conn: Mutex::new(Connection::new_system()?),
                active: Path::from("/"),
                restore: None,
                ssid: ssid.clone(),
                password: password.clone(),
                gateway: *gateway,
                frequency: *frequency,
                interface: "external".into(),
            });
        }
        if !zone_exists(zone()) {
            return Err(anyhow!(
                "firewalld zone '{}' is missing; run the one-time hotspot setup",
                zone()
            ));
        }

        let conn = Connection::new_system().context("system D-Bus")?;
        let (device, interface, restore) = pick_device(&conn)?;
        info!("Hotspot: using {interface} (takes over current connection: {})", restore.is_some());

        let ssid = format!("FQShare-{}", random_string(6));
        let password = random_string(16);

        let mut last_err = anyhow!("no band attempted");
        for (band, channel, frequency) in BANDS {
            match activate(&conn, &device, &ssid, &password, band, channel) {
                Ok(active) => {
                    let gateway = read_gateway(&conn, &device).unwrap_or(Ipv4Addr::new(10, 42, 0, 1));
                    info!("Hotspot: up on {interface}, {frequency} MHz, gateway {gateway}");
                    return Ok(Self {
                        conn: Mutex::new(conn),
                        active,
                        restore,
                        ssid,
                        password,
                        gateway,
                        frequency,
                        interface,
                    });
                }
                Err(e) => {
                    warn!("Hotspot: {band} channel {channel} failed: {e}");
                    last_err = e;
                }
            }
        }

        // Nothing came up: put the previous connection back.
        if let Some((connection, device)) = &restore {
            let _ = reactivate(&conn, connection, device);
        }
        Err(last_err)
    }

    /// Tear the hotspot down and restore the previous Wi-Fi connection.
    pub async fn stop(self) {
        let _ = tokio::task::spawn_blocking(move || drop(self)).await;
    }
}

impl Drop for Hotspot {
    fn drop(&mut self) {
        // `stop()` drops on a blocking thread; a plain drop (early return,
        // panic) does the same work inline. NM's bind-activation also cleans
        // up if the whole process goes away.
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        teardown(&conn, &self.active, self.restore.as_ref());
    }
}

fn teardown(conn: &Connection, active: &Path<'static>, restore: Option<&(Path<'static>, Path<'static>)>) {
    if &**active != "/" {
        let nm = conn.with_proxy(NM, NM_PATH, TIMEOUT);
        let r: Result<(), dbus::Error> = nm.method_call(NM, "DeactivateConnection", (active.clone(),));
        match r {
            Ok(()) => info!("Hotspot: stopped"),
            Err(e) => warn!("Hotspot: deactivate failed: {e}"),
        }
    }
    if let Some((connection, device)) = restore {
        match reactivate(conn, connection, device) {
            Ok(()) => info!("Hotspot: previous Wi-Fi connection restored"),
            Err(e) => warn!("Hotspot: couldn't restore previous connection: {e}"),
        }
    }
}

fn reactivate(conn: &Connection, connection: &Path<'static>, device: &Path<'static>) -> Result<(), anyhow::Error> {
    let nm = conn.with_proxy(NM, NM_PATH, TIMEOUT);
    let _: (Path<'static>,) =
        nm.method_call(NM, "ActivateConnection", (connection.clone(), device.clone(), Path::from("/")))?;
    Ok(())
}

/// Prefer an idle AP-capable Wi-Fi interface (no disruption); otherwise take
/// over the active one and remember what to restore.
fn pick_device(
    conn: &Connection,
) -> Result<(Path<'static>, String, Option<(Path<'static>, Path<'static>)>), anyhow::Error> {
    let nm = conn.with_proxy(NM, NM_PATH, TIMEOUT);
    let (devices,): (Vec<Path<'static>>,) = nm.method_call(NM, "GetDevices", ())?;

    let mut idle = None;
    let mut busy = None;
    for path in devices {
        let dev = conn.with_proxy(NM, path.clone(), TIMEOUT);
        let dtype: u32 = dev.get(NM_DEVICE, "DeviceType").unwrap_or(0);
        if dtype != NM_DEVICE_TYPE_WIFI {
            continue;
        }
        let caps: u32 = dev.get(NM_WIRELESS, "WirelessCapabilities").unwrap_or(0);
        if caps & NM_WIFI_DEVICE_CAP_AP == 0 {
            continue;
        }
        let iface: String = dev.get(NM_DEVICE, "Interface").unwrap_or_default();
        let state: u32 = dev.get(NM_DEVICE, "State").unwrap_or(0);
        if state == NM_DEVICE_STATE_DISCONNECTED && idle.is_none() {
            idle = Some((path, iface));
        } else if state == NM_DEVICE_STATE_ACTIVATED && busy.is_none() {
            busy = Some((path, iface));
        }
    }

    if let Some((path, iface)) = idle {
        return Ok((path, iface, None));
    }
    let (path, iface) = busy.ok_or_else(|| anyhow!("no AP-capable Wi-Fi interface"))?;
    let dev = conn.with_proxy(NM, path.clone(), TIMEOUT);
    let active: Path<'static> = dev.get(NM_DEVICE, "ActiveConnection")?;
    let restore = if &*active != "/" {
        let ac = conn.with_proxy(NM, active, TIMEOUT);
        let connection: Path<'static> = ac.get(NM_ACTIVE, "Connection")?;
        Some((connection, path.clone()))
    } else {
        None
    };
    Ok((path, iface, restore))
}

fn activate(
    conn: &Connection,
    device: &Path<'static>,
    ssid: &str,
    password: &str,
    band: &str,
    channel: u32,
) -> Result<Path<'static>, anyhow::Error> {
    let mut settings: HashMap<&str, PropMap> = HashMap::new();
    settings.insert(
        "connection",
        props(vec![
            ("id", Box::new(format!("FQShare {ssid}"))),
            ("type", Box::new("802-11-wireless".to_string())),
            ("autoconnect", Box::new(false)),
            ("zone", Box::new(zone().to_string())),
        ]),
    );
    settings.insert(
        "802-11-wireless",
        props(vec![
            ("ssid", Box::new(ssid.as_bytes().to_vec())),
            ("mode", Box::new("ap".to_string())),
            ("band", Box::new(band.to_string())),
            ("channel", Box::new(channel)),
        ]),
    );
    settings.insert(
        "802-11-wireless-security",
        props(vec![
            ("key-mgmt", Box::new("wpa-psk".to_string())),
            ("psk", Box::new(password.to_string())),
            ("proto", Box::new(vec!["rsn".to_string()])),
            ("pairwise", Box::new(vec!["ccmp".to_string()])),
            ("group", Box::new(vec!["ccmp".to_string()])),
        ]),
    );
    settings.insert("ipv4", props(vec![("method", Box::new("shared".to_string()))]));
    settings.insert("ipv6", props(vec![("method", Box::new("disabled".to_string()))]));

    let options = props(vec![
        ("persist", Box::new("volatile".to_string())),
        ("bind-activation", Box::new("dbus-client".to_string())),
    ]);

    let nm: Proxy<&Connection> = conn.with_proxy(NM, NM_PATH, Duration::from_secs(30));
    let (_path, active, _result): (Path<'static>, Path<'static>, PropMap) = nm.method_call(
        NM,
        "AddAndActivateConnection2",
        (settings, device.clone(), Path::from("/"), options),
    )?;

    // Wait for the AP to come up (or fail).
    let deadline = Instant::now() + Duration::from_secs(20);
    let ac = conn.with_proxy(NM, active.clone(), TIMEOUT);
    loop {
        let state: u32 = ac.get(NM_ACTIVE, "State").unwrap_or(NM_ACTIVE_STATE_DEACTIVATED);
        if state == NM_ACTIVE_STATE_ACTIVATED {
            return Ok(active);
        }
        if state == NM_ACTIVE_STATE_DEACTIVATED || Instant::now() > deadline {
            let _: Result<(), dbus::Error> =
                nm.method_call(NM, "DeactivateConnection", (active.clone(),));
            return Err(anyhow!("activation failed (state {state})"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn read_gateway(conn: &Connection, device: &Path<'static>) -> Option<Ipv4Addr> {
    let dev = conn.with_proxy(NM, device.clone(), TIMEOUT);
    let ip4: Path<'static> = dev.get(NM_DEVICE, "Ip4Config").ok()?;
    let cfg = conn.with_proxy(NM, ip4, TIMEOUT);
    let data: Vec<PropMap> = cfg.get(NM_IP4, "AddressData").ok()?;
    let addr = data.first()?.get("address")?.0.as_str()?.to_string();
    addr.parse().ok()
}

/// Read-only check through firewalld's D-Bus API (no privileges needed).
fn zone_exists(zone: &str) -> bool {
    let Ok(conn) = Connection::new_system() else {
        return false;
    };
    let fw = conn.with_proxy("org.fedoraproject.FirewallD1", "/org/fedoraproject/FirewallD1", TIMEOUT);
    let r: Result<(Vec<String>,), dbus::Error> =
        fw.method_call("org.fedoraproject.FirewallD1.zone", "getZones", ());
    match r {
        Ok((zones,)) => zones.iter().any(|z| z == zone),
        // No firewalld: nothing filters the hotspot, so nothing to open.
        Err(e) if e.name() == Some("org.freedesktop.DBus.Error.ServiceUnknown") => true,
        Err(_) => false,
    }
}

fn props(items: Vec<(&str, Box<dyn RefArg>)>) -> PropMap {
    items
        .into_iter()
        .map(|(k, v)| (k.to_string(), Variant(v)))
        .collect()
}

fn random_string(len: usize) -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

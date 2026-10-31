//! FQShare: receive Quick Share transfers from an Android phone with no shared
//! network. The phone finds the laptop over Bluetooth LE; large transfers move
//! to Wi-Fi (the same network if both are on it, otherwise a short-lived
//! laptop hotspot).

#[macro_use]
extern crate log;

mod config;
mod desktop;
mod tray;

use std::collections::HashMap;
use std::time::Duration;

use rqs_lib::channel::{ChannelMessage, Message, TransferAction, TransferKind};
use rqs_lib::hdl::info::{TransferMetadata, TransferPayload, TransferPayloadKind};
use rqs_lib::{RQS, TransferState, Visibility};
use tokio::sync::mpsc;
use tokio::time::Instant;

use desktop::{Desktop, Event, Note, escape};
use tray::Command;

const BUS_NAME: &str = "io.github.fqshare.FQShare";
const BUS_PATH: &str = "/io/github/fqshare/FQShare";

/// Session-bus interface used by a second launch of the app.
struct Remote {
    tx: mpsc::UnboundedSender<Command>,
}

#[zbus::interface(name = "io.github.fqshare.FQShare")]
impl Remote {
    /// Become visible to phones for the configured time.
    fn receive(&self) {
        let _ = self.tx.send(Command::Receive);
    }
}

/// Unanswered requests are declined after this long.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(60);
/// Don't hide in the middle of a transfer; check again after this long.
const HIDE_RETRY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Transfer {
    notification: u32,
    sender: String,
    waiting: bool,
    consent_deadline: Option<Instant>,
    last_progress: Option<Instant>,
}

struct App {
    cfg: config::Config,
    rqs: RQS,
    desktop: Desktop,
    tray: tray::Handle,
    visible_until: Option<Instant>,
    transfers: HashMap<String, Transfer>,
    /// Notification id → transfer id (for button presses).
    by_notification: HashMap<u32, String>,
    /// Notification id → what its buttons act on, after a transfer ended.
    results: HashMap<u32, Done>,
    hotspot_ready: bool,
}

enum Done {
    Files(Vec<std::path::PathBuf>),
    Text(String),
    Url(String),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,mdns_sd=error,bluez_async=error,btleplug=error,zbus=warn".into()
            }),
        )
        .without_time() // journald adds timestamps
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    // One instance per session. A second launch (e.g. from the app menu)
    // asks the running one to start receiving, then exits.
    let bus = zbus::Connection::session().await?;
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    bus.object_server().at(BUS_PATH, Remote { tx: cmd_tx.clone() }).await?;
    let flags = zbus::fdo::RequestNameFlags::DoNotQueue.into();
    match bus.request_name_with_flags(BUS_NAME, flags).await {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {}
        _ => {
            info!("FQShare is already running; asking it to receive");
            bus.call_method(Some(BUS_NAME), BUS_PATH, Some(BUS_NAME), "Receive", &()).await?;
            return Ok(());
        }
    }

    let cfg = config::load()?;
    std::fs::create_dir_all(&cfg.download_dir)?;
    info!("FQShare: name \u{201c}{}\u{201d}, saving to {}", cfg.device_name, cfg.download_dir.display());

    #[cfg(debug_assertions)]
    if let Ok(zone) = std::env::var("FQSHARE_TEST_HOTSPOT_ZONE") {
        rqs_lib::hdl::set_hotspot_zone_for_testing(&zone);
    }

    let mut rqs = RQS::new(
        Visibility::Invisible,
        None,
        Some(cfg.download_dir.clone()),
        Some(cfg.device_name.clone()),
    );
    // Phone and laptop talk directly; nothing is announced on the network.
    rqs.set_lan_receive(false);
    rqs.run().await?;

    let hotspot_ready = rqs_lib::hdl::hotspot_setup_done().await;
    if !hotspot_ready {
        warn!("Hotspot setup not done: big transfers stay on Bluetooth unless the phone shares our network");
    }

    let (desktop, events) = Desktop::connect().await?;
    let tray = tray::spawn(cfg.device_name.clone(), cfg.visible_minutes, hotspot_ready, cmd_tx).await?;

    let mut app = App {
        cfg,
        rqs,
        desktop,
        tray,
        visible_until: None,
        transfers: HashMap::new(),
        by_notification: HashMap::new(),
        results: HashMap::new(),
        hotspot_ready,
    };

    if std::env::args().any(|a| a == "--receive") {
        app.set_receiving(true).await;
    }

    app.run(cmd_rx, events).await;
    app.rqs.stop().await;
    Ok(())
}

impl App {
    async fn run(&mut self, mut cmds: mpsc::UnboundedReceiver<Command>, mut events: mpsc::UnboundedReceiver<Event>) {
        let mut messages = self.rqs.message_sender.subscribe();
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        let mut tick = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                Some(cmd) = cmds.recv() => match cmd {
                    Command::ToggleReceive => {
                        info!("Tray: toggle receiving");
                        let on = self.visible_until.is_none();
                        self.set_receiving(on).await;
                    }
                    Command::Receive => {
                        info!("Launcher: receive");
                        self.set_receiving(true).await;
                    }
                    Command::OpenFolder => self.desktop.show(&[], &self.cfg.download_dir).await,
                    Command::Quit => break,
                },
                Ok(msg) = messages.recv() => self.on_message(msg).await,
                Some(ev) = events.recv() => self.on_desktop_event(ev).await,
                _ = tick.tick() => self.on_tick().await,
                _ = tokio::signal::ctrl_c() => break,
                _ = term.recv() => break,
            }
        }
        info!("FQShare: quitting");
    }

    async fn set_receiving(&mut self, on: bool) {
        if on {
            let until = Instant::now() + Duration::from_secs(self.cfg.visible_minutes * 60);
            self.visible_until = Some(until);
            self.rqs.change_visibility(Visibility::Visible);
            info!("Receiving: visible for {} min", self.cfg.visible_minutes);
        } else {
            self.visible_until = None;
            self.rqs.change_visibility(Visibility::Invisible);
            info!("Receiving: hidden");
        }
        self.update_tray().await;
    }

    async fn update_tray(&self) {
        let left = self
            .visible_until
            .map(|u| u.saturating_duration_since(Instant::now()).as_secs().div_ceil(60))
            .unwrap_or(0);
        let receiving = self.visible_until.is_some();
        self.tray
            .update(move |t| {
                t.receiving = receiving;
                t.minutes_left = left;
            })
            .await;
    }

    async fn on_tick(&mut self) {
        let now = Instant::now();

        // Visibility window.
        if let Some(until) = self.visible_until {
            if now >= until {
                if self.transfers.values().any(|t| !t.waiting) {
                    // Hiding stops the Bluetooth link; wait for transfers to end.
                    self.visible_until = Some(now + HIDE_RETRY);
                } else {
                    self.set_receiving(false).await;
                }
            } else if now.duration_since(until - Duration::from_secs(self.cfg.visible_minutes * 60)).as_secs() % 30 == 0 {
                self.update_tray().await;
            }
        }

        // Unanswered consent requests.
        let expired: Vec<String> = self
            .transfers
            .iter()
            .filter(|(_, t)| t.waiting && t.consent_deadline.is_some_and(|d| now >= d))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            info!("Transfer {id}: no answer, declining");
            self.respond(&id, false).await;
        }
    }

    async fn on_message(&mut self, msg: ChannelMessage) {
        let Message::Client(client) = msg.msg else { return };
        if client.kind != TransferKind::Inbound {
            return;
        }
        let Some(state) = client.state else { return };
        let id = msg.id;
        let meta = client.metadata;

        match state {
            TransferState::WaitingForUserConsent => {
                if let Some(meta) = meta {
                    self.ask_consent(&id, &meta).await;
                }
            }
            TransferState::ReceivingFiles => {
                if let Some(meta) = meta {
                    self.progress(&id, &meta).await;
                }
            }
            TransferState::Finished => self.finished(&id, meta).await,
            TransferState::Rejected | TransferState::Cancelled | TransferState::Disconnected => {
                self.failed(&id, &state).await
            }
            _ => {}
        }
    }

    async fn ask_consent(&mut self, id: &str, meta: &TransferMetadata) {
        let sender = meta
            .source
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "A nearby device".into());
        let what = describe(meta);
        let pin = meta.pin_code.clone().unwrap_or_default();
        let mut body = format!("{}\nPIN {}", escape(&what), escape(&pin));
        if meta.total_bytes >= 1_000_000 && !self.hotspot_ready {
            body.push_str("\n(Bluetooth only: slow. Run the hotspot setup for fast transfers.)");
        }
        let summary = format!("{} wants to send", sender);
        let notification = self
            .desktop
            .notify(Note {
                replaces: 0,
                icon: "smartphone",
                summary: &escape(&summary),
                body: &body,
                actions: &[("accept", "Accept"), ("decline", "Decline")],
                resident: true,
                critical: true,
                timeout_ms: CONSENT_TIMEOUT.as_millis() as i32,
            })
            .await;
        info!("Transfer {id}: {sender} wants to send {what} (PIN {pin})");
        self.by_notification.insert(notification, id.to_string());
        self.transfers.insert(
            id.to_string(),
            Transfer {
                notification,
                sender,
                waiting: true,
                consent_deadline: Some(Instant::now() + CONSENT_TIMEOUT),
                last_progress: None,
            },
        );

        #[cfg(debug_assertions)]
        if std::env::var_os("FQSHARE_TEST_AUTO_ACCEPT").is_some() {
            warn!("TESTING: auto-accepting {id}");
            self.respond(id, true).await;
        }
    }

    async fn respond(&mut self, id: &str, accept: bool) {
        let Some(t) = self.transfers.get_mut(id) else { return };
        if !t.waiting {
            return;
        }
        t.waiting = false;
        t.consent_deadline = None;
        let notification = t.notification;
        let _ = self.rqs.message_sender.send(ChannelMessage {
            id: id.to_string(),
            msg: Message::Lib {
                action: if accept { TransferAction::ConsentAccept } else { TransferAction::ConsentDecline },
            },
        });
        if accept {
            let summary = format!("Receiving from {}", escape(&t.sender));
            self.desktop
                .notify(Note {
                    replaces: notification,
                    icon: "download",
                    summary: &summary,
                    body: "Starting\u{2026}",
                    actions: &[],
                    resident: true,
                    critical: false,
                    timeout_ms: 0,
                })
                .await;
        } else {
            self.desktop.close(notification).await;
            self.forget(id);
        }
    }

    async fn progress(&mut self, id: &str, meta: &TransferMetadata) {
        let Some(t) = self.transfers.get_mut(id) else { return };
        let now = Instant::now();
        if t.last_progress.is_some_and(|p| now.duration_since(p) < Duration::from_millis(700)) {
            return;
        }
        t.last_progress = Some(now);
        if meta.total_bytes == 0 {
            return;
        }
        let pct = (meta.ack_bytes * 100 / meta.total_bytes).min(100);
        let summary = format!("Receiving from {}", escape(&t.sender));
        let body = format!("{pct}% of {}", human_size(meta.total_bytes));
        let notification = t.notification;
        self.desktop
            .notify(Note {
                replaces: notification,
                icon: "download",
                summary: &summary,
                body: &body,
                actions: &[],
                resident: true,
                critical: false,
                timeout_ms: 0,
            })
            .await;
    }

    async fn finished(&mut self, id: &str, meta: Option<TransferMetadata>) {
        let Some(t) = self.transfers.remove(id) else { return };
        self.by_notification.remove(&t.notification);
        let sender = escape(&t.sender);

        let (summary, body, actions, done): (String, String, Vec<(&str, &str)>, Option<Done>) =
            match meta.as_ref().and_then(|m| m.payload.clone()) {
                Some(TransferPayload::Files(names)) => {
                    let paths: Vec<_> = names
                        .iter()
                        .map(|n| self.cfg.download_dir.join(n))
                        .filter(|p| p.exists())
                        .collect();
                    let list = names.iter().map(|n| escape(n)).collect::<Vec<_>>().join("\n");
                    (
                        format!("Received from {sender}"),
                        list,
                        vec![("show", "Show in folder")],
                        Some(Done::Files(paths)),
                    )
                }
                Some(TransferPayload::Url(url)) => (
                    format!("Link from {sender}"),
                    escape(&url),
                    vec![("open", "Open"), ("copy", "Copy")],
                    Some(Done::Url(url)),
                ),
                Some(TransferPayload::Text(text)) => (
                    format!("Text from {sender}"),
                    escape(&text),
                    vec![("copy", "Copy")],
                    Some(Done::Text(text)),
                ),
                Some(TransferPayload::Wifi { ssid, .. }) => (
                    format!("Wi-Fi network from {sender}"),
                    format!("{} (not joined automatically)", escape(&ssid)),
                    vec![],
                    None,
                ),
                None => (format!("Received from {sender}"), String::new(), vec![], None),
            };

        let notification = self
            .desktop
            .notify(Note {
                replaces: t.notification,
                icon: "dialog-ok",
                summary: &summary,
                body: &body,
                actions: &actions,
                resident: false,
                critical: false,
                timeout_ms: 30_000,
            })
            .await;
        if let Some(done) = done {
            self.results.insert(notification, done);
        }
        info!("Transfer {id}: finished");
    }

    async fn failed(&mut self, id: &str, state: &TransferState) {
        let Some(t) = self.transfers.remove(id) else { return };
        self.by_notification.remove(&t.notification);
        if t.waiting {
            // Phone gave up before an answer.
            self.desktop.close(t.notification).await;
            return;
        }
        let why = match state {
            TransferState::Cancelled => "was cancelled",
            TransferState::Rejected => "was declined",
            _ => "was interrupted",
        };
        let summary = format!("Transfer from {} {why}", escape(&t.sender));
        self.desktop
            .notify(Note {
                replaces: t.notification,
                icon: "dialog-warning",
                summary: &summary,
                body: "",
                actions: &[],
                resident: false,
                critical: false,
                timeout_ms: 15_000,
            })
            .await;
        info!("Transfer {id}: {why}");
    }

    async fn on_desktop_event(&mut self, ev: Event) {
        match ev {
            Event::Action { id, key } => {
                if let Some(tid) = self.by_notification.get(&id).cloned() {
                    match key.as_str() {
                        "accept" => self.respond(&tid, true).await,
                        "decline" => self.respond(&tid, false).await,
                        _ => {}
                    }
                    return;
                }
                match (self.results.get(&id), key.as_str()) {
                    (Some(Done::Files(paths)), "show") => {
                        self.desktop.show(paths, &self.cfg.download_dir).await
                    }
                    (Some(Done::Url(url)), "open") => self.desktop.open_url(url).await,
                    (Some(Done::Url(s)) | Some(Done::Text(s)), "copy") => {
                        if !self.desktop.copy(s).await {
                            warn!("clipboard unavailable");
                        }
                    }
                    _ => {}
                }
            }
            Event::Closed { id } => {
                self.results.remove(&id);
                // A consent request dismissed without an answer means no.
                if let Some(tid) = self.by_notification.get(&id).cloned() {
                    if self.transfers.get(&tid).is_some_and(|t| t.waiting) {
                        self.respond(&tid, false).await;
                    }
                }
            }
        }
    }

    fn forget(&mut self, id: &str) {
        if let Some(t) = self.transfers.remove(id) {
            self.by_notification.remove(&t.notification);
        }
    }
}

fn describe(meta: &TransferMetadata) -> String {
    match meta.payload_kind {
        TransferPayloadKind::Files => {
            let names = match &meta.payload {
                Some(TransferPayload::Files(n)) => n.clone(),
                _ => vec![],
            };
            let size = human_size(meta.total_bytes);
            match names.len() {
                0 => format!("files ({size})"),
                1 => format!("{} ({size})", names[0]),
                n => format!("{n} files: {}, \u{2026} ({size})", names[0]),
            }
        }
        TransferPayloadKind::Text => "a text".into(),
        TransferPayloadKind::Url => "a link".into(),
        TransferPayloadKind::WiFi => "a Wi-Fi network".into(),
    }
}

fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

//! Desktop integration over the session bus: notifications with buttons,
//! clipboard (Klipper), and showing files in the file manager.

use std::collections::HashMap;

use futures::StreamExt;
use tokio::sync::mpsc;
use zbus::zvariant::Value;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

#[derive(Debug)]
pub enum Event {
    Action { id: u32, key: String },
    Closed { id: u32 },
}

pub struct Desktop {
    conn: zbus::Connection,
    notifications: NotificationsProxy<'static>,
}

/// A notification to show. `body` must already be escaped with [`escape`].
pub struct Note<'a> {
    pub replaces: u32,
    pub icon: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    pub actions: &'a [(&'a str, &'a str)],
    /// Stays until the user acts on it.
    pub resident: bool,
    pub critical: bool,
    pub timeout_ms: i32,
}

impl Desktop {
    pub async fn connect() -> anyhow::Result<(Self, mpsc::UnboundedReceiver<Event>)> {
        let conn = zbus::Connection::session().await?;
        let notifications = NotificationsProxy::new(&conn).await?;

        let (tx, rx) = mpsc::unbounded_channel();
        let mut actions = notifications.receive_action_invoked().await?;
        let mut closed = notifications.receive_notification_closed().await?;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(sig) = actions.next() => {
                        if let Ok(a) = sig.args() {
                            let _ = tx.send(Event::Action { id: a.id, key: a.action_key });
                        }
                    }
                    Some(sig) = closed.next() => {
                        if let Ok(a) = sig.args() {
                            let _ = tx.send(Event::Closed { id: a.id });
                        }
                    }
                    else => break,
                }
            }
        });

        Ok((Self { conn, notifications }, rx))
    }

    pub async fn notify(&self, n: Note<'_>) -> u32 {
        let mut actions = Vec::new();
        for (key, label) in n.actions {
            actions.push(*key);
            actions.push(*label);
        }
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from("fqshare"));
        if n.resident {
            hints.insert("resident", Value::from(true));
        }
        if n.critical {
            hints.insert("urgency", Value::from(2u8));
        }
        match self
            .notifications
            .notify("FQShare", n.replaces, n.icon, n.summary, n.body, &actions, hints, n.timeout_ms)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!("notification failed: {e}");
                0
            }
        }
    }

    pub async fn close(&self, id: u32) {
        if id != 0 {
            let _ = self.notifications.close_notification(id).await;
        }
    }

    /// Put text on the clipboard through Klipper (KDE). Returns false if
    /// Klipper isn't running.
    pub async fn copy(&self, text: &str) -> bool {
        self.conn
            .call_method(
                Some("org.kde.klipper"),
                "/klipper",
                Some("org.kde.klipper.klipper"),
                "setClipboardContents",
                &(text,),
            )
            .await
            .is_ok()
    }

    /// Open the file manager on `paths` (selected) or on `folder`.
    pub async fn show(&self, paths: &[std::path::PathBuf], folder: &std::path::Path) {
        let (method, uris): (&str, Vec<String>) = if paths.is_empty() {
            ("ShowFolders", vec![file_uri(folder)])
        } else {
            ("ShowItems", paths.iter().map(|p| file_uri(p)).collect())
        };
        let r = self
            .conn
            .call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                method,
                &(uris, ""),
            )
            .await;
        if let Err(e) = r {
            warn!("couldn't open the file manager: {e}");
        }
    }
}

impl Desktop {
    /// Open an http(s) link in the default browser through the desktop
    /// portal, so the browser runs outside this service's sandbox. Anything
    /// that isn't a plain http(s) URL is refused.
    pub async fn open_url(&self, url: &str) {
        let ok = (url.starts_with("https://") || url.starts_with("http://"))
            && !url.chars().any(|c| c.is_control() || c.is_whitespace());
        if !ok {
            warn!("refusing to open non-http link");
            return;
        }
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let r = self
            .conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.OpenURI"),
                "OpenURI",
                &("", url, options),
            )
            .await;
        if let Err(e) = r {
            warn!("couldn't open link: {e}");
        }
    }
}

/// Notification bodies may contain markup; escape anything the phone sent.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

fn file_uri(p: &std::path::Path) -> String {
    let mut s = String::from("file://");
    for b in p.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

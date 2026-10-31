//! Talks to the running FQShare service over the session bus.

use crate::state::Snapshot;

const NAME: &str = "io.github.fqshare.FQShare";
const PATH: &str = "/io/github/fqshare/FQShare";

#[derive(Clone)]
pub struct Client {
    conn: zbus::Connection,
}

impl Client {
    /// Connect, starting the systemd user service if it isn't running.
    pub async fn connect() -> anyhow::Result<Self> {
        let conn = zbus::Connection::session().await?;
        let client = Self { conn };
        if client.state().await.is_err() {
            let _ = tokio::process::Command::new("systemctl")
                .args(["--user", "start", "fqshare.service"])
                .status()
                .await;
            client.state().await?;
        }
        Ok(client)
    }

    pub async fn state(&self) -> anyhow::Result<Snapshot> {
        let reply = self
            .conn
            .call_method(Some(NAME), PATH, Some(NAME), "GetState", &())
            .await?;
        let json: String = reply.body().deserialize()?;
        Ok(serde_json::from_str(&json)?)
    }

    pub async fn call(&self, method: &str) {
        let _ = self.conn.call_method(Some(NAME), PATH, Some(NAME), method, &()).await;
    }

    pub async fn call_id(&self, method: &str, id: &str) {
        let _ = self.conn.call_method(Some(NAME), PATH, Some(NAME), method, &(id,)).await;
    }

    /// Clipboard through Klipper; false if it isn't available.
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

    pub async fn show_in_folder(&self, path: &str) {
        let uri = format!("file://{}", percent(path));
        let _ = self
            .conn
            .call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![uri], ""),
            )
            .await;
    }
}

fn percent(p: &str) -> String {
    let mut s = String::new();
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

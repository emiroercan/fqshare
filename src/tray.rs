//! System tray icon (StatusNotifierItem). Left click toggles receiving.

use ksni::TrayMethods;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub enum Command {
    ToggleReceive,
    Receive,
    OpenFolder,
    Quit,
}

pub struct Tray {
    pub device_name: String,
    pub receiving: bool,
    pub minutes_left: u64,
    pub visible_minutes: u64,
    pub hotspot_ready: bool,
    tx: mpsc::UnboundedSender<Command>,
}

pub type Handle = ksni::Handle<Tray>;

pub async fn spawn(
    device_name: String,
    visible_minutes: u64,
    hotspot_ready: bool,
    tx: mpsc::UnboundedSender<Command>,
) -> anyhow::Result<Handle> {
    let tray = Tray {
        device_name,
        receiving: false,
        minutes_left: 0,
        visible_minutes,
        hotspot_ready,
        tx,
    };
    Ok(tray.spawn().await?)
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "fqshare".into()
    }

    fn title(&self) -> String {
        "FQShare".into()
    }

    fn icon_name(&self) -> String {
        if self.receiving { "download" } else { "smartphone" }.into()
    }

    fn status(&self) -> ksni::Status {
        if self.receiving { ksni::Status::Active } else { ksni::Status::Passive }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = if self.receiving {
            format!(
                "Visible to nearby phones as \u{201c}{}\u{201d} for {} more min",
                self.device_name, self.minutes_left
            )
        } else {
            "Hidden. Click to receive from your phone.".into()
        };
        ksni::ToolTip {
            title: "FQShare".into(),
            description,
            icon_name: self.icon_name(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Command::ToggleReceive);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: if self.receiving {
                    format!("Stop receiving ({} min left)", self.minutes_left)
                } else {
                    format!("Receive for {} minutes", self.visible_minutes)
                },
                icon_name: if self.receiving { "media-playback-stop" } else { "download" }.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Command::ToggleReceive);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open received files".into(),
                icon_name: "folder-download".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Command::OpenFolder);
                }),
                ..Default::default()
            }
            .into(),
        ];
        if !self.hotspot_ready {
            items.push(MenuItem::Separator);
            items.push(
                StandardItem {
                    label: "Fast transfers need the one-time hotspot setup".into(),
                    icon_name: "dialog-information".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Command::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

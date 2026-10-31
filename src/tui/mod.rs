//! `fqshare tui`: a full-screen terminal front end for the running service.
//!
//! Mouse and keyboard, live progress, big PIN, image previews (kitty/sixel
//! graphics where the terminal supports them, half-blocks otherwise), small
//! transitions (tachyonfx), synchronized output to avoid flicker.

mod client;
mod view;

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use futures::StreamExt;
use ratatui::layout::{Position, Rect};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tachyonfx::Effect;

use crate::state::{HistoryItem, ItemKind, Phase, Snapshot};
use client::Client;

/// Something clickable on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    ToggleReceive,
    Accept(String),
    Decline(String),
    OpenFolder,
    RestartBluetooth,
    Quit,
    Select(usize),
    Open,
    ShowInFolder,
    Copy,
}

pub struct App {
    client: Client,
    snap: Snapshot,
    connected: bool,
    selected: usize,
    /// Clickable regions from the last frame.
    hits: Vec<(Rect, Action)>,
    hover: Option<Position>,
    /// Smoothed speed per transfer: (last bytes, last time, bytes/s).
    speed: HashMap<String, (u64, Instant, f64)>,
    picker: Picker,
    preview: Option<(String, Preview)>,
    loading: Option<String>,
    effects: Vec<(Region, Effect)>,
    last_frame: Instant,
    started: Instant,
    toast: Option<(String, Instant)>,
    seen_requests: std::collections::HashSet<String>,
    seen_history: u64,
    confirm_bt: bool,
    quit: bool,
}

pub enum Preview {
    Image(StatefulProtocol),
    NotImage,
    Failed,
}

/// Where an effect is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    Screen,
    Incoming,
    FirstRecent,
}

pub async fn run() -> anyhow::Result<()> {
    let client = match Client::connect().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FQShare service isn't reachable: {e}\nStart it with: systemctl --user start fqshare");
            std::process::exit(1);
        }
    };
    // Ask the terminal what it can draw (kitty / sixel / iTerm2 images),
    // before taking over the screen.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;

    let snap = client.state().await.unwrap_or_default();
    let mut app = App {
        client,
        seen_history: snap.history.first().map(|h| h.when).unwrap_or(0),
        seen_requests: snap.transfers.iter().map(|t| t.id.clone()).collect(),
        snap,
        connected: true,
        selected: 0,
        hits: vec![],
        hover: None,
        speed: HashMap::new(),
        picker,
        preview: None,
        loading: None,
        effects: vec![(Region::Screen, view::intro_effect())],
        last_frame: Instant::now(),
        started: Instant::now(),
        toast: None,
        confirm_bt: false,
        quit: false,
    };

    let result = app.run(&mut terminal).await;

    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

impl App {
    async fn run(&mut self, terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
        let mut events = EventStream::new();
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        let mut frame = tokio::time::interval(Duration::from_millis(33));
        let (img_tx, mut img_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Preview)>();

        while !self.quit {
            self.request_preview(&img_tx);
            self.draw(terminal)?;

            tokio::select! {
                Some(ev) = events.next() => {
                    if let Ok(ev) = ev {
                        self.on_event(ev).await;
                    }
                }
                _ = poll.tick() => self.refresh().await,
                Some((path, p)) = img_rx.recv() => {
                    if self.loading.as_deref() == Some(path.as_str()) {
                        self.loading = None;
                    }
                    self.preview = Some((path, p));
                }
                // Only spin at frame rate while something animates.
                _ = frame.tick(), if self.animating() => {}
            }
        }
        Ok(())
    }

    fn animating(&self) -> bool {
        !self.effects.is_empty()
            || self.snap.visible_secs > 0
            || !self.snap.transfers.is_empty()
            || self.toast.is_some()
    }

    fn draw(&mut self, terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
        // Synchronized output: the terminal shows the frame all at once.
        let mut out = std::io::stdout();
        let _ = crossterm::queue!(out, crossterm::terminal::BeginSynchronizedUpdate);
        let elapsed = self.last_frame.elapsed();
        self.last_frame = Instant::now();
        terminal.draw(|f| view::render(self, f, elapsed))?;
        let _ = crossterm::queue!(out, crossterm::terminal::EndSynchronizedUpdate);
        let _ = out.flush();
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed() > Duration::from_secs(3)) {
            self.toast = None;
        }
        Ok(())
    }

    async fn refresh(&mut self) {
        match self.client.state().await {
            Ok(s) => {
                self.connected = true;
                // New request → animate the card in and ring the bell.
                for t in &s.transfers {
                    if t.phase == Phase::Asking && self.seen_requests.insert(t.id.clone()) {
                        self.effects.push((Region::Incoming, view::request_effect()));
                        print!("\x07");
                    }
                }
                // Newly received item → select and highlight it.
                if let Some(first) = s.history.first() {
                    if first.when > self.seen_history {
                        self.seen_history = first.when;
                        self.selected = 0;
                        self.effects.push((Region::FirstRecent, view::new_item_effect()));
                    }
                }
                let now = Instant::now();
                for t in &s.transfers {
                    let e = self.speed.entry(t.id.clone()).or_insert((t.ack_bytes, now, 0.0));
                    let dt = now.duration_since(e.1).as_secs_f64();
                    if dt > 0.4 {
                        let inst = (t.ack_bytes.saturating_sub(e.0)) as f64 / dt;
                        e.2 = if e.2 == 0.0 { inst } else { e.2 * 0.6 + inst * 0.4 };
                        e.0 = t.ack_bytes;
                        e.1 = now;
                    }
                }
                self.speed.retain(|id, _| s.transfers.iter().any(|t| &t.id == id));
                if self.selected >= s.history.len() {
                    self.selected = s.history.len().saturating_sub(1);
                }
                self.snap = s;
            }
            Err(_) => self.connected = false,
        }
    }

    pub fn selected_item(&self) -> Option<&HistoryItem> {
        self.snap.history.get(self.selected)
    }

    /// Load the selected item's image in the background.
    fn request_preview(&mut self, tx: &tokio::sync::mpsc::UnboundedSender<(String, Preview)>) {
        let Some(path) = self
            .selected_item()
            .filter(|h| h.kind == ItemKind::Files)
            .and_then(|h| h.files.first().cloned())
        else {
            return;
        };
        if self.preview.as_ref().is_some_and(|(p, _)| *p == path) || self.loading.as_deref() == Some(&path) {
            return;
        }
        self.loading = Some(path.clone());
        let picker = self.picker.clone();
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let p = if !view::is_image(&path) {
                Preview::NotImage
            } else {
                match image::ImageReader::open(&path)
                    .and_then(|r| r.with_guessed_format())
                    .map_err(|_| ())
                    .and_then(|r| r.decode().map_err(|_| ()))
                {
                    Ok(img) => Preview::Image(picker.new_resize_protocol(img.thumbnail(1024, 1024))),
                    Err(_) => Preview::Failed,
                }
            };
            let _ = tx.send((path, p));
        });
    }

    async fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let asking = self
                    .snap
                    .transfers
                    .iter()
                    .find(|t| t.phase == Phase::Asking)
                    .map(|t| t.id.clone());
                let action = match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
                    KeyCode::Char('r') | KeyCode::Char(' ') => Some(Action::ToggleReceive),
                    KeyCode::Char('a') | KeyCode::Char('y') => asking.clone().map(Action::Accept),
                    KeyCode::Char('d') | KeyCode::Char('n') => asking.map(Action::Decline),
                    KeyCode::Char('f') => Some(Action::OpenFolder),
                    KeyCode::Char('b') => Some(Action::RestartBluetooth),
                    KeyCode::Char('o') | KeyCode::Enter => Some(Action::Open),
                    KeyCode::Char('s') => Some(Action::ShowInFolder),
                    KeyCode::Char('c') => Some(Action::Copy),
                    KeyCode::Up | KeyCode::Char('k') => Some(Action::Select(self.selected.saturating_sub(1))),
                    KeyCode::Down | KeyCode::Char('j') => Some(Action::Select(self.selected + 1)),
                    KeyCode::Home => Some(Action::Select(0)),
                    KeyCode::End => Some(Action::Select(usize::MAX)),
                    _ => None,
                };
                if let Some(a) = action {
                    self.act(a).await;
                }
            }
            Event::Mouse(m) => {
                let pos = Position::new(m.column, m.row);
                self.hover = Some(pos);
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let hit = self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, a)| a.clone());
                        if let Some(a) = hit {
                            self.act(a).await;
                        }
                    }
                    MouseEventKind::ScrollUp => self.act(Action::Select(self.selected.saturating_sub(1))).await,
                    MouseEventKind::ScrollDown => self.act(Action::Select(self.selected + 1)).await,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    async fn act(&mut self, a: Action) {
        if a != Action::RestartBluetooth {
            self.confirm_bt = false;
        }
        match a {
            Action::Quit => self.quit = true,
            Action::ToggleReceive => self.client.call("ToggleReceive").await,
            Action::Accept(id) => self.client.call_id("Accept", &id).await,
            Action::Decline(id) => self.client.call_id("Decline", &id).await,
            Action::OpenFolder => self.client.call("OpenFolder").await,
            Action::RestartBluetooth => {
                // Restarting drops every Bluetooth device for a moment: ask twice.
                if self.confirm_bt {
                    self.confirm_bt = false;
                    self.client.call("RestartBluetooth").await;
                    self.toast("Restarting Bluetooth\u{2026} (password prompt)");
                } else {
                    self.confirm_bt = true;
                    self.toast("Press B again to restart Bluetooth");
                }
            }
            Action::Select(i) => {
                let n = self.snap.history.len();
                if n > 0 {
                    self.selected = i.min(n - 1);
                }
            }
            Action::Open => {
                if let Some(item) = self.selected_item().cloned() {
                    match item.kind {
                        ItemKind::Files => {
                            if let Some(p) = item.files.first() {
                                open_external(p);
                            }
                        }
                        ItemKind::Link => {
                            if item.text.starts_with("https://") || item.text.starts_with("http://") {
                                open_external(&item.text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Action::ShowInFolder => {
                if let Some(p) = self.selected_item().and_then(|h| h.files.first().cloned()) {
                    self.client.show_in_folder(&p).await;
                }
            }
            Action::Copy => {
                let text = self.selected_item().map(|h| match h.kind {
                    ItemKind::Files => h.files.join("\n"),
                    _ => h.text.clone(),
                });
                if let Some(text) = text {
                    if !self.client.copy(&text).await {
                        osc52_copy(&text);
                    }
                    self.toast("Copied");
                }
            }
        }
        self.refresh().await;
    }

    fn toast(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), Instant::now()));
    }
}

fn open_external(target: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Terminal clipboard (OSC 52) when Klipper isn't there.
fn osc52_copy(text: &str) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    print!("\x1b]52;c;{b64}\x07");
    let _ = std::io::stdout().flush();
}

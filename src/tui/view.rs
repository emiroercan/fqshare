//! Drawing. Everything clickable registers a hit box in `app.hits`.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Gauge, LineGauge, Paragraph, Wrap};
use ratatui_image::StatefulImage;
use tachyonfx::{Effect, Interpolation, fx};
use tui_big_text::{BigText, PixelSize};

use super::{Action, App, Preview, Region};
use crate::state::{HistoryItem, ItemKind, Phase, Snapshot, TransferView};

// Palette (true colour). Works on dark terminals; text uses the terminal's
// default foreground so light themes stay readable.
const ACCENT: Color = Color::Rgb(122, 162, 247);
const GREEN: Color = Color::Rgb(158, 206, 106);
const AMBER: Color = Color::Rgb(224, 175, 104);
const RED: Color = Color::Rgb(247, 118, 142);
const MUTED: Color = Color::Rgb(125, 131, 160);
const BORDER: Color = Color::Rgb(68, 75, 106);
const SELECT_BG: Color = Color::Rgb(41, 46, 66);
const ON_COLOR: Color = Color::Rgb(26, 27, 38);

pub fn intro_effect() -> Effect {
    fx::coalesce((450, Interpolation::QuadOut))
}

pub fn request_effect() -> Effect {
    fx::parallel(&[
        fx::coalesce((380, Interpolation::QuadOut)),
        fx::fade_from_fg(AMBER, (900, Interpolation::SineOut)),
    ])
}

pub fn new_item_effect() -> Effect {
    fx::fade_from_fg(GREEN, (1400, Interpolation::QuadOut))
}

pub fn is_image(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".webp", ".gif", ".bmp"].iter().any(|e| p.ends_with(e))
}

pub fn render(app: &mut App, f: &mut Frame, elapsed: Duration) {
    app.hits.clear();
    let snap = app.snap.clone();
    let area = f.area();

    let [header, body, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(app, f, header, &snap);

    let wide = body.width >= 96;
    let (incoming, recent, preview) = if wide {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(body);
        let [r1, r2] = Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(right);
        (left, r1, Some(r2))
    } else {
        let inc_h = if snap.transfers.is_empty() { 9 } else { 16 }.min(body.height / 2 + 2);
        let [a, b, c] = Layout::vertical([
            Constraint::Length(inc_h),
            Constraint::Min(5),
            Constraint::Percentage(if body.height > 30 { 40 } else { 0 }),
        ])
        .areas(body);
        (a, b, (c.height >= 6).then_some(c))
    };

    render_incoming(app, f, incoming, &snap);
    let first_row = render_recent(app, f, recent, &snap);
    if let Some(p) = preview {
        render_preview(app, f, p, &snap);
    }
    render_footer(app, f, footer);

    if !app.connected {
        let w = area.width.min(54);
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, 3);
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new("FQShare service isn't responding \u{2014} retrying\u{2026}")
                .alignment(Alignment::Center)
                .block(panel("").border_style(Style::new().fg(RED))),
            r,
        );
    }
    if let Some((msg, _)) = &app.toast {
        let w = (msg.chars().count() as u16 + 6).min(area.width);
        let r = Rect::new(area.right().saturating_sub(w + 1), area.bottom().saturating_sub(4), w, 3);
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(msg.as_str()).alignment(Alignment::Center).block(
                Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)),
            ),
            r,
        );
    }

    // Transitions on top of the finished frame.
    let elapsed: tachyonfx::Duration = elapsed.into();
    let buf = f.buffer_mut();
    app.effects.retain_mut(|(region, effect)| {
        let r = match region {
            Region::Screen => area,
            Region::Incoming => incoming,
            Region::FirstRecent => first_row.unwrap_or_default(),
        };
        if r.area() > 0 {
            effect.process(elapsed, buf, r);
        }
        !effect.done()
    });
}

fn panel(title: &str) -> Block<'_> {
    let b = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(BORDER))
        .padding(ratatui::widgets::Padding::horizontal(1));
    if title.is_empty() {
        b
    } else {
        b.title(Line::from(vec![" ".into(), Span::styled(title, Style::new().fg(ACCENT).bold()), " ".into()]))
    }
}

fn hovered(app: &App, r: Rect) -> bool {
    app.hover.is_some_and(|p| r.contains(p))
}

/// A pill-shaped button: ` label  key `. Returns its width.
fn button(app: &mut App, f: &mut Frame, x: u16, y: u16, label: &str, key: &str, color: Color, filled: bool, action: Action) -> u16 {
    let text = format!(" {label} ");
    let keycap = format!(" {key} ");
    let w = (text.chars().count() + keycap.chars().count()) as u16;
    let r = Rect::new(x, y, w, 1);
    let hot = hovered(app, r);
    let (fg, bg) = if filled || hot { (ON_COLOR, color) } else { (color, Color::Reset) };
    let main = Style::new().fg(fg).bg(bg).add_modifier(if hot { Modifier::BOLD } else { Modifier::empty() });
    let key_style = Style::new().fg(if filled || hot { ON_COLOR } else { MUTED }).bg(if filled || hot { color } else { SELECT_BG });
    let line = Line::from(vec![Span::styled(text, main), Span::styled(keycap, key_style.add_modifier(Modifier::DIM))]);
    let area = f.area();
    let r = r.intersection(area);
    f.render_widget(Paragraph::new(line), r);
    app.hits.push((r, action));
    w
}

fn render_header(app: &mut App, f: &mut Frame, area: Rect, snap: &Snapshot) {
    let [top, rule] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let left = Line::from(vec![
        Span::styled(" \u{25c6} ", Style::new().fg(ACCENT).bold()),
        Span::styled("FQShare", Style::new().bold()),
        Span::styled("  \u{00b7}  ", Style::new().fg(MUTED)),
        Span::raw(snap.device_name.clone()),
    ]);
    f.render_widget(Paragraph::new(left), top);

    // Right side: Wi-Fi badge + visibility pill (clickable).
    let t = app.started.elapsed().as_millis();
    let (pill, color) = if snap.visible_secs > 0 {
        let dot = if (t / 600) % 2 == 0 { "\u{25cf}" } else { "\u{25c9}" };
        (format!(" {dot} VISIBLE  {} ", mmss(snap.visible_secs)), GREEN)
    } else {
        (" \u{25cb} HIDDEN ".to_string(), MUTED)
    };
    let badge = if snap.hotspot_ready {
        Span::styled(" Wi-Fi ready ", Style::new().fg(MUTED))
    } else {
        Span::styled(" Wi-Fi setup needed ", Style::new().fg(AMBER))
    };
    let pill_w = pill.chars().count() as u16;
    let badge_w = badge.width() as u16;
    let x = top.right().saturating_sub(pill_w + badge_w + 1);
    f.render_widget(Paragraph::new(Line::from(badge)), Rect::new(x, top.y, badge_w, 1).intersection(top));
    let pr = Rect::new(x + badge_w, top.y, pill_w, 1).intersection(top);
    let hot = hovered(app, pr);
    f.render_widget(
        Paragraph::new(Span::styled(pill, Style::new().fg(ON_COLOR).bg(color).bold().add_modifier(if hot { Modifier::REVERSED } else { Modifier::empty() }))),
        pr,
    );
    app.hits.push((pr, Action::ToggleReceive));

    f.render_widget(
        Paragraph::new("\u{2500}".repeat(rule.width as usize)).style(Style::new().fg(BORDER)),
        rule,
    );
}

fn render_incoming(app: &mut App, f: &mut Frame, area: Rect, snap: &Snapshot) {
    let asking = snap.transfers.iter().find(|t| t.phase == Phase::Asking);
    let receiving: Vec<&TransferView> = snap.transfers.iter().filter(|t| t.phase == Phase::Receiving).collect();
    let title = if asking.is_some() {
        "Incoming request"
    } else if !receiving.is_empty() {
        "Receiving"
    } else {
        "Incoming"
    };
    let block = panel(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(t) = asking {
        render_request(app, f, inner, t);
    } else if !receiving.is_empty() {
        render_progress(app, f, inner, &receiving);
    } else {
        render_idle(app, f, inner, snap);
    }
}

fn render_request(app: &mut App, f: &mut Frame, area: Rect, t: &TransferView) {
    let mut lines = vec![
        Line::from(Span::styled(t.sender.clone(), Style::new().bold().fg(AMBER))),
        Line::from(vec![Span::styled("wants to send ", Style::new().fg(MUTED)), Span::raw(t.what.clone())]),
    ];
    for name in t.files.iter().take(3) {
        lines.push(Line::from(vec![
            Span::styled("  \u{2022} ", Style::new().fg(MUTED)),
            Span::raw(file_name(name)),
        ]));
    }
    if t.files.len() > 3 {
        lines.push(Line::from(Span::styled(format!("    + {} more", t.files.len() - 3), Style::new().fg(MUTED))));
    }
    let info_h = lines.len() as u16;

    let pin_big = area.width >= 36 && area.height >= info_h + 11;
    let pin_h = if pin_big { 4 } else { 1 };
    let [info, _, pin_label, pin, hint, _, gauge, _, buttons] = Layout::vertical([
        Constraint::Length(info_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(pin_h),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    f.render_widget(Paragraph::new(lines), info);
    f.render_widget(Paragraph::new(Span::styled("PIN", Style::new().fg(MUTED))), pin_label);
    if pin_big {
        let big = BigText::builder()
            .pixel_size(PixelSize::HalfHeight)
            .style(Style::new().fg(AMBER).bold())
            .lines(vec![t.pin.clone().into()])
            .build();
        f.render_widget(big, pin);
    } else {
        f.render_widget(Paragraph::new(Span::styled(t.pin.clone(), Style::new().fg(AMBER).bold())), pin);
    }
    f.render_widget(
        Paragraph::new(Span::styled("Check it matches the PIN on your phone.", Style::new().fg(MUTED).italic())),
        hint,
    );

    let ratio = (t.answer_secs as f64 / 60.0).clamp(0.0, 1.0);
    f.render_widget(
        LineGauge::default()
            .ratio(ratio)
            .label(Span::styled(format!("declines in {}s ", t.answer_secs), Style::new().fg(MUTED)))
            .filled_style(Style::new().fg(if t.answer_secs > 15 { ACCENT } else { RED }))
            .unfilled_style(Style::new().fg(BORDER)),
        gauge,
    );

    let x = buttons.x;
    let w = button(app, f, x, buttons.y, "\u{2713} Accept", "A", GREEN, true, Action::Accept(t.id.clone()));
    button(app, f, x + w + 2, buttons.y, "\u{2715} Decline", "D", RED, false, Action::Decline(t.id.clone()));
}

fn render_progress(app: &mut App, f: &mut Frame, area: Rect, items: &[&TransferView]) {
    let mut y = area.y;
    for t in items.iter().take(((area.height / 5).max(1)) as usize) {
        let r = Rect::new(area.x, y, area.width, 5.min(area.bottom().saturating_sub(y)));
        if r.height < 3 {
            break;
        }
        let [who, bar, detail] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)]).areas(r);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(t.sender.clone(), Style::new().bold()),
                Span::styled("  \u{2192}  ", Style::new().fg(MUTED)),
                Span::raw(t.what.clone()),
            ])),
            who,
        );
        let ratio = if t.total_bytes > 0 { t.ack_bytes as f64 / t.total_bytes as f64 } else { 0.0 };
        f.render_widget(
            Gauge::default()
                .ratio(ratio.clamp(0.0, 1.0))
                .label(Span::styled(
                    format!("{:.0}%  \u{00b7}  {} of {}", ratio * 100.0, size(t.ack_bytes), size(t.total_bytes)),
                    Style::new().fg(Color::White).bold(),
                ))
                .gauge_style(Style::new().fg(if t.via_wifi { GREEN } else { ACCENT }).bg(SELECT_BG))
                .use_unicode(true),
            bar,
        );
        let speed = app.speed.get(&t.id).map(|s| s.2).unwrap_or(0.0);
        let left = if speed > 1.0 {
            let secs = (t.total_bytes.saturating_sub(t.ack_bytes)) as f64 / speed;
            format!("  \u{00b7}  {} left", duration_words(secs as u64))
        } else {
            String::new()
        };
        let via = if t.via_wifi {
            Span::styled("\u{26a1} Wi-Fi", Style::new().fg(GREEN).bold())
        } else {
            Span::styled("\u{25cc} Bluetooth", Style::new().fg(ACCENT))
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                via,
                Span::styled(format!("  \u{00b7}  {}/s{left}", size(speed as u64)), Style::new().fg(MUTED)),
            ])),
            detail,
        );
        y += 5;
    }
}

fn render_idle(app: &mut App, f: &mut Frame, area: Rect, snap: &Snapshot) {
    let t = app.started.elapsed().as_millis() as u64;
    if snap.visible_secs > 0 {
        // A small "radar": rings pulse outward around the dot.
        let phase = (t / 220) % 6;
        let ring = |n: u64| {
            let lit = phase == n || phase == n + 1;
            Style::new().fg(if lit { GREEN } else { BORDER })
        };
        let radar = Line::from(vec![
            Span::styled("( ", ring(3)),
            Span::styled("( ", ring(2)),
            Span::styled("( ", ring(1)),
            Span::styled("\u{25cf}", Style::new().fg(GREEN).bold()),
            Span::styled(" )", ring(1)),
            Span::styled(" )", ring(2)),
            Span::styled(" )", ring(3)),
        ]);
        let text = Text::from(vec![
            radar,
            Line::raw(""),
            Line::from(vec![Span::styled("Visible as ", Style::new().fg(MUTED)), Span::styled(snap.device_name.clone(), Style::new().bold())]),
            Line::raw(""),
            Line::from(Span::styled("On your phone: Share \u{2192} Quick Share \u{2192} pick this laptop", Style::new().fg(MUTED))),
            Line::from(Span::styled(format!("Hides in {}", mmss(snap.visible_secs)), Style::new().fg(MUTED))),
        ]);
        let h = text.height() as u16;
        let r = centered(area, area.width, h);
        f.render_widget(Paragraph::new(text).alignment(Alignment::Center).wrap(Wrap { trim: true }), r);
    } else {
        let text = Text::from(vec![
            Line::from(Span::styled("\u{25cb}", Style::new().fg(MUTED).bold())),
            Line::raw(""),
            Line::from(Span::styled("Hidden from phones", Style::new().bold())),
            Line::from(Span::styled(
                format!("Become visible for {} minutes to receive.", snap.visible_minutes),
                Style::new().fg(MUTED),
            )),
            Line::raw(""),
        ]);
        let h = text.height() as u16 + 1;
        let r = centered(area, area.width, h);
        f.render_widget(Paragraph::new(text).alignment(Alignment::Center), r);
        let label = "Receive";
        let w = (label.len() + 2 + 3) as u16;
        let bx = r.x + r.width.saturating_sub(w) / 2;
        button(app, f, bx, r.bottom().saturating_sub(1), label, "R", ACCENT, true, Action::ToggleReceive);
    }
}

/// Returns the rect of the first row (for the "new item" highlight).
fn render_recent(app: &mut App, f: &mut Frame, area: Rect, snap: &Snapshot) -> Option<Rect> {
    let block = panel("Recent");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if snap.history.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled("Nothing received yet.", Style::new().fg(MUTED).italic())),
            inner,
        );
        return None;
    }
    let rows = (inner.height / 2).max(1) as usize;
    let offset = app.selected.saturating_sub(rows - 1);
    let mut first = None;
    for (i, item) in snap.history.iter().enumerate().skip(offset).take(rows) {
        let y = inner.y + ((i - offset) as u16) * 2;
        let r = Rect::new(inner.x.saturating_sub(1), y, inner.width + 2, 2.min(inner.bottom() - y));
        if i == 0 {
            first = Some(r);
        }
        let sel = i == app.selected;
        let hot = hovered(app, r);
        if sel || hot {
            f.render_widget(Block::new().style(Style::new().bg(if sel { SELECT_BG } else { Color::Rgb(33, 36, 52) })), r);
        }
        let (icon, color) = kind_icon(item);
        let title = item_title(item);
        let sz = if item.total_bytes > 0 { size(item.total_bytes) } else { String::new() };
        let bar = Span::styled(if sel { "\u{258c}" } else { " " }, Style::new().fg(ACCENT));
        let maxw = (r.width as usize).saturating_sub(sz.len() + 6);
        let l1 = Line::from(vec![
            bar.clone(),
            Span::styled(format!("{icon} "), Style::new().fg(color)),
            Span::styled(truncate(&title, maxw), Style::new().add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() })),
        ]);
        let via = if item.kind == ItemKind::Failed {
            ""
        } else if item.via_wifi {
            " \u{00b7} Wi-Fi"
        } else {
            " \u{00b7} Bluetooth"
        };
        let l2 = Line::from(vec![
            bar,
            Span::styled(format!("  {} \u{00b7} {}{via}", item.sender, when(item.when)), Style::new().fg(MUTED)),
        ]);
        f.render_widget(Paragraph::new(vec![l1, l2]), r);
        if !sz.is_empty() {
            let sr = Rect::new(r.right().saturating_sub(sz.len() as u16 + 2), y, sz.len() as u16 + 1, 1);
            f.render_widget(Paragraph::new(Span::styled(sz, Style::new().fg(MUTED))), sr);
        }
        app.hits.push((r, Action::Select(i)));
    }
    first
}

fn render_preview(app: &mut App, f: &mut Frame, area: Rect, snap: &Snapshot) {
    let block = panel("Preview");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(item) = snap.history.get(app.selected).cloned() else {
        f.render_widget(
            Paragraph::new(Span::styled("Received files, texts and links show up here.", Style::new().fg(MUTED).italic())),
            inner,
        );
        return;
    };
    let [content, _, actions] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    match item.kind {
        ItemKind::Files => {
            let path = item.files.first().cloned().unwrap_or_default();
            let showing_image = matches!(&app.preview, Some((p, Preview::Image(_))) if *p == path);
            if showing_image {
                let [img, meta] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(content);
                if let Some((_, Preview::Image(proto))) = app.preview.as_mut() {
                    f.render_stateful_widget(StatefulImage::default(), img.inner(Margin::new(0, 0)), proto);
                }
                f.render_widget(Paragraph::new(file_meta(&item, &path)).style(Style::new().fg(MUTED)), meta);
            } else {
                let loading = app.loading.as_deref() == Some(path.as_str());
                let mut lines = vec![
                    Line::from(Span::styled(file_name(&path), Style::new().bold())),
                    Line::from(Span::styled(file_meta(&item, &path), Style::new().fg(MUTED))),
                ];
                if item.files.len() > 1 {
                    lines.push(Line::raw(""));
                    for p in item.files.iter().skip(1).take(6) {
                        lines.push(Line::from(vec![Span::styled("\u{2022} ", Style::new().fg(MUTED)), Span::raw(file_name(p))]));
                    }
                }
                if loading {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(Span::styled("Loading preview\u{2026}", Style::new().fg(MUTED).italic())));
                }
                f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), content);
            }
            let mut x = actions.x;
            x += button(app, f, x, actions.y, "Open", "O", ACCENT, false, Action::Open) + 1;
            x += button(app, f, x, actions.y, "Show in folder", "S", ACCENT, false, Action::ShowInFolder) + 1;
            button(app, f, x, actions.y, "Copy path", "C", ACCENT, false, Action::Copy);
        }
        ItemKind::Text => {
            f.render_widget(Paragraph::new(item.text.clone()).wrap(Wrap { trim: false }), content);
            button(app, f, actions.x, actions.y, "Copy", "C", ACCENT, false, Action::Copy);
        }
        ItemKind::Link => {
            f.render_widget(
                Paragraph::new(Span::styled(item.text.clone(), Style::new().fg(ACCENT).underlined())).wrap(Wrap { trim: false }),
                content,
            );
            let w = button(app, f, actions.x, actions.y, "Open", "O", ACCENT, false, Action::Open);
            button(app, f, actions.x + w + 1, actions.y, "Copy", "C", ACCENT, false, Action::Copy);
        }
        ItemKind::Wifi => {
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![Span::styled("Wi-Fi network ", Style::new().fg(MUTED)), Span::styled(item.text.clone(), Style::new().bold())]),
                    Line::from(Span::styled("Not joined automatically.", Style::new().fg(MUTED))),
                ]),
                content,
            );
        }
        ItemKind::Failed => {
            let mut lines = vec![Line::from(Span::styled(format!("\u{2715} Transfer {}", item.text), Style::new().fg(RED).bold()))];
            for p in item.files.iter().take(6) {
                lines.push(Line::from(vec![Span::styled("\u{2022} ", Style::new().fg(MUTED)), Span::raw(file_name(p))]));
            }
            f.render_widget(Paragraph::new(lines), content);
        }
    }
}

fn render_footer(app: &mut App, f: &mut Frame, area: Rect) {
    let receiving = app.snap.visible_secs > 0;
    let mut x = area.x + 1;
    let items: [(&str, &str, Action); 4] = [
        (if receiving { "Hide" } else { "Receive" }, "R", Action::ToggleReceive),
        ("Folder", "F", Action::OpenFolder),
        (if app.confirm_bt { "Confirm restart" } else { "Restart Bluetooth" }, "B", Action::RestartBluetooth),
        ("Quit", "Q", Action::Quit),
    ];
    for (label, key, action) in items {
        let color = if action == Action::RestartBluetooth && app.confirm_bt { AMBER } else { MUTED };
        x += button(app, f, x, area.y, label, key, color, false, action) + 1;
        if x >= area.right() {
            break;
        }
    }
    let hint = "\u{2191}\u{2193} select  \u{00b7}  mouse works";
    let hw = hint.chars().count() as u16;
    if x + hw + 2 < area.right() {
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::new().fg(BORDER))),
            Rect::new(area.right() - hw - 1, area.y, hw, 1),
        );
    }
}

// --- helpers ---------------------------------------------------------------

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn kind_icon(item: &HistoryItem) -> (&'static str, Color) {
    match item.kind {
        ItemKind::Files if item.files.first().is_some_and(|p| is_image(p)) => ("\u{25a3}", GREEN),
        ItemKind::Files => ("\u{25a4}", ACCENT),
        ItemKind::Text => ("\u{00b6}", AMBER),
        ItemKind::Link => ("\u{2197}", ACCENT),
        ItemKind::Wifi => ("\u{25c8}", ACCENT),
        ItemKind::Failed => ("\u{2715}", RED),
    }
}

fn item_title(item: &HistoryItem) -> String {
    match item.kind {
        ItemKind::Files | ItemKind::Failed => match item.files.len() {
            0 => "Files".into(),
            1 => file_name(&item.files[0]),
            n => format!("{} + {} more", file_name(&item.files[0]), n - 1),
        },
        ItemKind::Text => item.text.lines().next().unwrap_or("").to_string(),
        ItemKind::Link | ItemKind::Wifi => item.text.clone(),
    }
}

fn file_name(p: &str) -> String {
    std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string())
}

fn file_meta(item: &HistoryItem, path: &str) -> String {
    let dir = std::path::Path::new(path).parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = if !home.is_empty() && dir.starts_with(&home) { dir.replacen(&home, "~", 1) } else { dir };
    format!("{} \u{00b7} {}", size(item.total_bytes), dir)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('\u{2026}');
        out
    }
}

fn size(b: u64) -> String {
    let f = b as f64;
    if f >= 1e9 {
        format!("{:.1} GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1} MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0} KB", f / 1e3)
    } else {
        format!("{b} B")
    }
}

fn mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn duration_words(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        format!("{} min", secs.div_ceil(60))
    } else {
        format!("{} h {} min", secs / 3600, (secs % 3600) / 60)
    }
}

fn when(unix: u64) -> String {
    use chrono::{Local, TimeZone};
    let Some(t) = Local.timestamp_opt(unix as i64, 0).single() else { return String::new() };
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else if (now.date_naive() - t.date_naive()).num_days() < 7 {
        t.format("%a %H:%M").to_string()
    } else {
        t.format("%d %b").to_string()
    }
}

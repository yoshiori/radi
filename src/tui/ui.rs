use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Sparkline};
use throbber_widgets_tui::{Throbber, ThrobberState};
use tui_big_text::{BigText, PixelSize};
use tui_popup::Popup;

use crate::app::{App, AppState, RecentRecording, format_size};
use crate::upload::progress::{UploadPhase, UploadProgress};

// Unified palette. Using named constants keeps state-specific tinting
// consistent across the timer, borders, status line and level meter.
const ACCENT: Color = Color::Cyan;
const ACCENT_DIM: Color = Color::DarkGray;
const REC: Color = Color::Red;
const OK: Color = Color::Green;
const WARN: Color = Color::Yellow;
const ERR: Color = Color::Red;
/// "Busy, input ignored" — worn by every state that swallows keypresses
/// (Processing, Uploading). Shares WARN's hue today, but the semantics
/// differ (busy vs. caution), so the two are kept as distinct constants.
const BUSY: Color = Color::Yellow;

/// Height below which the big timer is skipped in favour of a plain line —
/// tui-big-text needs ~8 rows plus the surrounding chrome to render legibly.
const BIG_TIMER_MIN_HEIGHT: u16 = 22;

pub fn render(frame: &mut Frame, app: &App) {
    // For ConfirmQuit, render the underlying state as a dimmed backdrop
    // and overlay the confirmation popup on top.
    if let AppState::ConfirmQuit { previous } = &app.state {
        render_main(frame, app, previous);
        render_quit_popup(frame, previous);
        return;
    }
    render_main(frame, app, &app.state);
}

fn render_quit_popup(frame: &mut Frame, previous: &AppState) {
    let question = if matches!(previous, AppState::Recording) {
        "Stop recording and quit?"
    } else {
        "Quit radi?"
    };
    let body = Text::from(vec![
        Line::from(question),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[y]",
                Style::default().fg(WARN).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Yes    "),
            Span::styled(
                "[n]",
                Style::default().fg(WARN).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Cancel"),
        ]),
    ]);
    let popup = Popup::new(body)
        .title(" Confirm ")
        .style(Style::new().fg(Color::White).bg(Color::Black));
    frame.render_widget(&popup, frame.area());
}

fn render_main(frame: &mut Frame, app: &App, state: &AppState) {
    let accent = state_accent(state);
    let use_big_timer = frame.area().height >= BIG_TIMER_MIN_HEIGHT;

    let timer_height: u16 = if use_big_timer { 9 } else { 3 };
    let chunks = Layout::vertical([
        Constraint::Length(3),            // header
        Constraint::Length(timer_height), // timer / upload status
        Constraint::Length(5),            // level meter (sparkline + gauge)
        Constraint::Min(0),               // spacer
        Constraint::Length(3),            // key hints
    ])
    .split(frame.area());

    render_header(frame, chunks[0], app, state, accent);
    render_center(frame, chunks[1], app, state, accent, use_big_timer);
    render_level(frame, chunks[2], app, state, accent);
    render_recent(frame, chunks[3], app, accent);
    render_hints(frame, chunks[4], state, accent);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, state: &AppState, accent: Color) {
    let (badge, badge_style) = status_badge(state);
    let device = app.device_name.as_deref().unwrap_or("No device");

    let title_line = Line::from(vec![
        Span::styled(" radi ", Style::default().fg(accent).bold()),
        Span::styled("· podcast recorder ", Style::default().fg(ACCENT_DIM)),
    ]);
    let info_line = Line::from(vec![
        Span::styled(badge, badge_style),
        Span::raw("  "),
        Span::styled("🎙 ", Style::default().fg(ACCENT_DIM)),
        Span::styled(device.to_string(), Style::default().fg(Color::Gray)),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(title_line);
    let para = Paragraph::new(info_line)
        .block(block)
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
}

fn render_center(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    state: &AppState,
    accent: Color,
    use_big_timer: bool,
) {
    match state {
        AppState::Uploading(path) => {
            render_upload_in_progress(frame, area, path, &app.upload_progress, accent)
        }
        AppState::Uploaded { webview_url, .. } => {
            render_upload_done(frame, area, webview_url, accent, use_big_timer)
        }
        AppState::UploadFailed { error, .. } => render_upload_failed(frame, area, error, accent),
        _ => render_timer(frame, area, app, state, accent, use_big_timer),
    }
}

fn render_timer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    state: &AppState,
    accent: Color,
    use_big_timer: bool,
) {
    let elapsed = format_duration(app.elapsed());
    let label = match state {
        AppState::Idle => "Ready",
        AppState::Recording => "Recording",
        AppState::Processing => "Processing…",
        AppState::Done(_) => "Saved",
        _ => "",
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(label, Style::default().fg(accent).bold()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if use_big_timer && inner.height >= 6 {
        let big = BigText::builder()
            .pixel_size(PixelSize::HalfHeight)
            .alignment(Alignment::Center)
            .style(Style::default().fg(accent).bold())
            .lines(vec![elapsed.into()])
            .build();
        frame.render_widget(big, inner);
    } else {
        let p = Paragraph::new(Line::from(Span::styled(
            elapsed,
            Style::default().fg(accent).bold(),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(p, inner);
    }
}

fn render_upload_in_progress(
    frame: &mut Frame,
    area: Rect,
    path: &std::path::Path,
    progress: &UploadProgress,
    accent: Color,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Uploading to LISTEN", Style::default().fg(accent).bold()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let spinner_span = upload_throbber_span(accent);
    let phase = progress.phase();
    let phase_label = match phase {
        UploadPhase::Preparing => "Preparing upload…",
        UploadPhase::Uploading => "Uploading",
        UploadPhase::Finalizing => "Finalizing episode…",
    };

    let file_line = Line::from(Span::styled(
        format!("file: {}", path.display()),
        Style::default().fg(ACCENT_DIM),
    ));

    match phase {
        UploadPhase::Uploading => {
            // status / gauge / spacer / file path. Spacer absorbs any slack
            // so the file path row is always the one that keeps its height
            // when the panel is short (e.g. timer_height == 3 on a tiny
            // terminal).
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(inner);

            let status = Paragraph::new(Line::from(vec![
                spinner_span,
                Span::raw(" "),
                Span::styled(
                    phase_label,
                    Style::default().fg(accent).add_modifier(Modifier::DIM),
                ),
            ]))
            .alignment(Alignment::Center);
            frame.render_widget(status, rows[0]);

            let uploaded = progress.uploaded();
            let total = progress.total();
            let ratio = progress.ratio().unwrap_or(0.0);
            let label = if total == 0 {
                "starting…".to_string()
            } else {
                format_upload_label(uploaded, total)
            };
            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(accent).bg(Color::Reset))
                .label(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
                .ratio(ratio);
            frame.render_widget(gauge, rows[1]);

            frame.render_widget(
                Paragraph::new(file_line).alignment(Alignment::Center),
                rows[3],
            );
        }
        UploadPhase::Preparing | UploadPhase::Finalizing => {
            // No useful bytes to show yet / anymore — keep the quick phases
            // minimal so they read as transitions, not stalls.
            let lines = vec![
                Line::from(vec![
                    spinner_span,
                    Span::raw(" "),
                    Span::styled(
                        phase_label,
                        Style::default().fg(accent).add_modifier(Modifier::DIM),
                    ),
                ]),
                Line::from(""),
                file_line,
            ];
            frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
        }
    }
}

/// Build the time-driven throbber used by the upload panel.
fn upload_throbber_span<'a>(accent: Color) -> Span<'a> {
    let step = ((std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        / 100) as i8)
        .rem_euclid(8);
    let mut throb_state = ThrobberState::default();
    throb_state.calc_step(step);
    let throb = Throbber::default()
        .throbber_set(throbber_widgets_tui::BRAILLE_EIGHT_DOUBLE)
        .style(Style::default().fg(accent));
    throb.to_symbol_span(&throb_state)
}

/// "48% · 2.3 MB / 4.8 MB" — a compact label for the progress gauge.
fn format_upload_label(uploaded: u64, total: u64) -> String {
    // `checked_div` avoids a separate `if total == 0` branch that clippy
    // flags as `manual_checked_ops` on newer toolchains.
    let pct = uploaded
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0)
        .min(100);
    format!(
        "{pct}% · {} / {}",
        format_size(uploaded),
        format_size(total)
    )
}

fn render_upload_done(
    frame: &mut Frame,
    area: Rect,
    webview_url: &str,
    accent: Color,
    use_big_timer: bool,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Uploaded", Style::default().fg(accent).bold()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if use_big_timer && inner.height >= 6 {
        let vertical = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(inner);
        let big = BigText::builder()
            .pixel_size(PixelSize::Sextant)
            .alignment(Alignment::Center)
            .style(Style::default().fg(accent).bold())
            .lines(vec!["✓ UPLOADED".into()])
            .build();
        frame.render_widget(big, vertical[0]);
        let url = Paragraph::new(Line::from(Span::styled(
            webview_url.to_string(),
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::UNDERLINED),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(url, vertical[1]);
    } else {
        let lines = vec![
            Line::from(Span::styled(
                "✓ Uploaded",
                Style::default().fg(accent).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                webview_url.to_string(),
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::UNDERLINED),
            )),
        ];
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
    }
}

fn render_upload_failed(frame: &mut Frame, area: Rect, error: &str, accent: Color) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Upload failed", Style::default().fg(ERR).bold()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = vec![
        Line::from(Span::styled(
            "✗ Upload failed",
            Style::default().fg(ERR).bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            error.to_string(),
            Style::default().fg(Color::Gray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

fn render_level(frame: &mut Frame, area: Rect, app: &App, state: &AppState, accent: Color) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Level", Style::default().fg(accent).bold()),
            Span::raw(" "),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    let level = app.peak().min(1.0) as f64;
    let gauge_color = if level > 0.8 {
        ERR
    } else if level > 0.5 {
        WARN
    } else {
        OK
    };
    let db_label = if level > 0.0005 {
        format!("{:>6.1} dB", 20.0 * level.log10())
    } else {
        "  -∞ dB".to_string()
    };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::Reset))
        .label(Span::styled(
            db_label,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .ratio(level);
    frame.render_widget(gauge, rows[0]);

    // Rolling waveform of recent peaks so Recording state actually feels alive.
    if rows[1].height > 0 && rows[1].width > 0 {
        let history = app.peak_history();
        let data: Vec<u64> = history
            .iter()
            .rev()
            .take(rows[1].width as usize)
            .rev()
            .map(|&v| (v.clamp(0.0, 1.0) * 100.0) as u64)
            .collect();
        let sparkline_color = match state {
            AppState::Recording => REC,
            AppState::Idle => ACCENT_DIM,
            _ => accent,
        };
        let sparkline = Sparkline::default()
            .data(&data)
            .max(100)
            .style(Style::default().fg(sparkline_color));
        frame.render_widget(sparkline, rows[1]);
    }
}

fn render_recent(frame: &mut Frame, area: Rect, app: &App, accent: Color) {
    if area.height == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT_DIM))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled("Recent", Style::default().fg(accent).bold()),
            Span::styled(
                format!(" ({}) ", app.recent().len()),
                Style::default().fg(ACCENT_DIM),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    let recent = app.recent();
    if recent.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No recordings yet — press [r] to start.",
            Style::default().fg(ACCENT_DIM),
        )))
        .alignment(Alignment::Center);
        frame.render_widget(empty, inner);
        return;
    }

    let current = app.output_path.as_path();
    let rows = inner.height as usize;
    let lines: Vec<Line> = recent
        .iter()
        .take(rows)
        .map(|r| format_recent_line(r, current == r.path))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_recent_line(rec: &RecentRecording, is_current: bool) -> Line<'_> {
    let name = rec.path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    let marker = if is_current { "▸ " } else { "  " };
    let marker_style = if is_current {
        Style::default().fg(REC).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ACCENT_DIM)
    };
    let name_style = if is_current {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    Line::from(vec![
        Span::styled(marker, marker_style),
        Span::styled(
            format!("{:<12}", rec.timestamp),
            Style::default().fg(ACCENT_DIM),
        ),
        Span::styled(
            format!("{:>9}  ", rec.size),
            Style::default().fg(ACCENT_DIM),
        ),
        Span::styled(name.to_string(), name_style),
    ])
}

fn render_hints(frame: &mut Frame, area: Rect, state: &AppState, accent: Color) {
    let hints: &[(&str, &str)] = match state {
        AppState::Idle => &[("r", " Record  "), ("q", " Quit")],
        AppState::Recording => &[("s", " Stop & Save  "), ("q", " Stop & Quit")],
        AppState::Processing => &[],
        AppState::Done(_) => &[
            ("u", " Upload to LISTEN  "),
            ("r", " New Recording  "),
            ("q", " Quit"),
        ],
        AppState::Uploading(_) => &[],
        AppState::Uploaded { .. } => &[
            ("o", " Open in browser  "),
            ("r", " New Recording  "),
            ("q", " Quit"),
        ],
        AppState::UploadFailed { .. } => &[
            ("u", " Retry Upload  "),
            ("r", " New Recording  "),
            ("q", " Quit"),
        ],
        AppState::ConfirmQuit { .. } => {
            unreachable!("ConfirmQuit is handled by render as a popup")
        }
    };

    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(ACCENT_DIM));

    let line = if hints.is_empty() {
        // Busy states: say explicitly that keypresses won't do anything,
        // and tint in BUSY so the row reads as part of the "input locked"
        // styling already applied to the header badge and borders.
        match state {
            AppState::Processing | AppState::Uploading(_) => Line::from(Span::styled(
                "— input locked —",
                Style::default().fg(BUSY).add_modifier(Modifier::DIM),
            )),
            _ => Line::from(Span::styled("", Style::default().fg(ACCENT_DIM))),
        }
    } else {
        let key_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
        let label_style = Style::default().fg(Color::Gray);
        let bracket_style = Style::default().fg(ACCENT_DIM);
        let mut spans = Vec::with_capacity(hints.len() * 4);
        for (key, label) in hints {
            spans.push(Span::styled("[", bracket_style));
            spans.push(Span::styled(*key, key_style));
            spans.push(Span::styled("]", bracket_style));
            spans.push(Span::styled(*label, label_style));
        }
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn status_badge(state: &AppState) -> (&'static str, Style) {
    let bold = Modifier::BOLD;
    match state {
        AppState::Idle => ("■ IDLE", Style::default().fg(ACCENT_DIM).add_modifier(bold)),
        AppState::Recording => (
            "● REC",
            Style::default().fg(Color::White).bg(REC).add_modifier(bold),
        ),
        AppState::Processing => ("◌ PROC", Style::default().fg(BUSY).add_modifier(bold)),
        AppState::Uploading(_) => ("⇪ UP", Style::default().fg(BUSY).add_modifier(bold)),
        AppState::Uploaded { .. } => ("✓ DONE", Style::default().fg(OK).add_modifier(bold)),
        AppState::UploadFailed { .. } => ("✗ FAIL", Style::default().fg(ERR).add_modifier(bold)),
        AppState::Done(_) => ("✓ SAVED", Style::default().fg(OK).add_modifier(bold)),
        AppState::ConfirmQuit { .. } => ("? QUIT", Style::default().fg(WARN).add_modifier(bold)),
    }
}

fn state_accent(state: &AppState) -> Color {
    // Processing and Uploading share BUSY on purpose: both ignore
    // keypresses, and a single "input-locked" color lets the user tell
    // at a glance that the TUI is doing work and typing won't help.
    match state {
        AppState::Idle => ACCENT_DIM,
        AppState::Recording => REC,
        AppState::Processing => BUSY,
        AppState::Done(_) => OK,
        AppState::Uploading(_) => BUSY,
        AppState::Uploaded { .. } => OK,
        AppState::UploadFailed { .. } => ERR,
        AppState::ConfirmQuit { .. } => ACCENT_DIM,
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_label_shows_percent_and_sizes() {
        // 1.5 MiB of 5.0 MiB → 30%
        let s = format_upload_label(1_572_864, 5_242_880);
        assert!(s.contains("30%"), "got: {s}");
        assert!(s.contains("1.5 MB"), "got: {s}");
        assert!(s.contains("5.0 MB"), "got: {s}");
    }

    #[test]
    fn upload_label_with_zero_total_is_zero_percent() {
        let s = format_upload_label(0, 0);
        assert!(s.starts_with("0%"), "got: {s}");
    }

    #[test]
    fn upload_label_clamps_over_100_percent() {
        // Defensively clamp in case a race lets uploaded briefly exceed total.
        let s = format_upload_label(10, 5);
        assert!(s.contains("100%"), "got: {s}");
    }
}

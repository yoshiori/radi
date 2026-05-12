use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Sparkline};
use throbber_widgets_tui::{Throbber, ThrobberState};
use tui_big_text::{BigText, PixelSize};
use tui_popup::Popup;

use crate::app::{App, AppState, RecentRecording, SyncState, format_size};
use crate::tui::splash;
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

/// Height below which the fancy ANSI Shadow banner is skipped in favour of
/// the original 3-row text header. The banner needs 6 rows + 1 info row +
/// 2 border rows = 9, and the rest of the main layout (timer + level + hints
/// + spacer) needs ≥18 rows after that, giving 27 total.
const BIG_HEADER_MIN_HEIGHT: u16 = 27;

/// Vertical budget for the banner-style header panel.
const BIG_HEADER_HEIGHT: u16 = 9;

/// Minimum width of the info column beside the banner before we fall back
/// to the stacked banner-above-info layout. 18 columns comfortably fits the
/// status badge and a short device name; longer device names will still
/// render but may be truncated, which is acceptable.
const HEADER_INFO_MIN_WIDTH: u16 = 18;

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
    let area = frame.area();
    let use_big_timer = area.height >= BIG_TIMER_MIN_HEIGHT;
    let use_big_header = area.height >= BIG_HEADER_MIN_HEIGHT;

    let header_height: u16 = if use_big_header { BIG_HEADER_HEIGHT } else { 3 };
    let timer_height: u16 = if use_big_timer { 9 } else { 3 };
    let chunks = Layout::vertical([
        Constraint::Length(header_height), // header
        Constraint::Length(timer_height),  // timer / upload status
        Constraint::Length(5),             // level meter (sparkline + gauge)
        Constraint::Min(0),                // spacer
        Constraint::Length(3),             // key hints
    ])
    .split(area);

    render_header(frame, chunks[0], app, state, accent, use_big_header);
    render_center(frame, chunks[1], app, state, accent, use_big_timer);
    render_level(frame, chunks[2], app, state, accent);
    render_recent(frame, chunks[3], app, accent);
    render_hints(frame, chunks[4], state, accent);
}

/// Return the exact rect where the big-header banner is drawn, matching the
/// layout used by `render_header_banner`. Used by the splash→header slide
/// transition to compute the landing position, and to `Clear` the slot so the
/// floating banner doesn't visually overlap a duplicate in the header.
/// Returns `None` if the current frame is not large enough for the banner
/// header (in which case the compact header is in use and has no slot).
pub(crate) fn header_banner_slot(frame_area: Rect) -> Option<Rect> {
    if frame_area.height < BIG_HEADER_MIN_HEIGHT {
        return None;
    }
    let inner_x = frame_area.x + 1;
    let inner_y = frame_area.y + 1;
    let inner_width = frame_area.width.saturating_sub(2);
    if inner_width < splash::BANNER_WIDTH {
        return None;
    }
    let banner_col_width = splash::BANNER_WIDTH + 2;
    let side_by_side = inner_width >= banner_col_width + HEADER_INFO_MIN_WIDTH;
    let slot_x = if side_by_side {
        // Banner is rendered at `cols[0].x + 1` inside the banner column,
        // giving one char of left padding before the first glyph.
        inner_x + 1
    } else {
        // Stacked: banner is centred horizontally across the full inner width.
        inner_x + (inner_width - splash::BANNER_WIDTH) / 2
    };
    Some(Rect::new(
        slot_x,
        inner_y,
        splash::BANNER_WIDTH,
        splash::BANNER_HEIGHT,
    ))
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    state: &AppState,
    accent: Color,
    use_big_header: bool,
) {
    if use_big_header {
        render_header_banner(frame, area, app, state, accent);
    } else {
        render_header_compact(frame, area, app, state, accent);
    }
}

fn render_header_compact(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    state: &AppState,
    accent: Color,
) {
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

fn render_header_banner(frame: &mut Frame, area: Rect, app: &App, state: &AppState, accent: Color) {
    let (badge, badge_style) = status_badge(state);
    let device = app.device_name.as_deref().unwrap_or("No device");

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let banner_col_width = splash::BANNER_WIDTH + 2;
    let side_by_side = inner.width >= banner_col_width + HEADER_INFO_MIN_WIDTH;

    if side_by_side {
        let cols = Layout::horizontal([Constraint::Length(banner_col_width), Constraint::Min(0)])
            .split(inner);

        // Position banner explicitly (1 char left pad inside banner column)
        // so `header_banner_slot` can compute the identical rect without
        // having to reverse Alignment::Center's offset math.
        let banner_rect = Rect::new(
            cols[0].x + 1,
            cols[0].y,
            splash::BANNER_WIDTH,
            splash::BANNER_HEIGHT.min(cols[0].height),
        );
        frame.render_widget(
            Paragraph::new(Text::from(splash::banner_lines(1.0))),
            banner_rect,
        );

        // Info column: subtitle / status / device, vertically centred-ish.
        let info_rows = Layout::vertical([
            Constraint::Length(1), // top pad
            Constraint::Length(1), // subtitle
            Constraint::Length(1), // status badge
            Constraint::Length(1), // device
            Constraint::Min(0),    // bottom pad
        ])
        .split(cols[1]);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "The TUI podcast recorder",
                Style::default().fg(ACCENT_DIM).add_modifier(Modifier::BOLD),
            ))),
            info_rows[1],
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(badge, badge_style))),
            info_rows[2],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("🎙 ", Style::default().fg(ACCENT_DIM)),
                Span::styled(device.to_string(), Style::default().fg(Color::Gray)),
            ])),
            info_rows[3],
        );
    } else {
        // Stacked: banner centred on top, single-line info on the bottom.
        let rows = Layout::vertical([
            Constraint::Length(splash::BANNER_HEIGHT),
            Constraint::Min(1),
        ])
        .split(inner);

        let pad = rows[0].width.saturating_sub(splash::BANNER_WIDTH) / 2;
        let banner_rect = Rect::new(
            rows[0].x + pad,
            rows[0].y,
            splash::BANNER_WIDTH.min(rows[0].width),
            splash::BANNER_HEIGHT.min(rows[0].height),
        );
        frame.render_widget(
            Paragraph::new(Text::from(splash::banner_lines(1.0))),
            banner_rect,
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(badge, badge_style),
                Span::raw("  "),
                Span::styled("🎙 ", Style::default().fg(ACCENT_DIM)),
                Span::styled(device.to_string(), Style::default().fg(Color::Gray)),
            ]))
            .alignment(Alignment::Center),
            rows[1],
        );
    }
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

/// Trailing spans for the Recent panel title that indicate the rehydrate
/// pass's current phase. Idle and Done collapse to no spans on purpose:
/// once the sidecars are up to date the indicator's job is done, and the
/// rehydrated titles speak for themselves in the rows below.
fn sync_indicator_spans(state: &SyncState) -> Vec<Span<'static>> {
    match state {
        SyncState::Idle | SyncState::Done { .. } => Vec::new(),
        SyncState::Syncing => vec![
            upload_throbber_span(ACCENT),
            Span::raw(" "),
            Span::styled(
                "syncing…",
                Style::default().fg(ACCENT_DIM).add_modifier(Modifier::DIM),
            ),
            Span::raw(" "),
        ],
        SyncState::Failed(_) => vec![
            Span::styled("⚠ ", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
            Span::styled(
                "sync failed",
                Style::default().fg(ACCENT_DIM).add_modifier(Modifier::DIM),
            ),
            Span::raw(" "),
        ],
    }
}

fn render_recent(frame: &mut Frame, area: Rect, app: &App, accent: Color) {
    if area.height == 0 {
        return;
    }
    let mut title_spans = vec![
        Span::raw(" "),
        Span::styled("Recent", Style::default().fg(accent).bold()),
        Span::styled(
            format!(" ({}) ", app.recent().len()),
            Style::default().fg(ACCENT_DIM),
        ),
    ];
    title_spans.extend(sync_indicator_spans(&app.sync_state()));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT_DIM))
        .title(Line::from(title_spans));
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

    let rows = inner.height as usize;
    let selected = app.selected_recent();
    let lines: Vec<Line> = recent
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, r)| format_recent_line(r, selected == Some(i)))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_recent_line(rec: &RecentRecording, is_selected: bool) -> Line<'_> {
    let filename = rec.path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
    // Prefer the LISTEN title once a sidecar exists — once a recording is
    // uploaded the human-meaningful name is the episode title, not the
    // timestamped filename. Falls back to the filename otherwise.
    let label = rec
        .episode
        .as_ref()
        .map(|e| e.title.as_str())
        .unwrap_or(filename);
    let marker = if is_selected { "▸ " } else { "  " };
    let marker_style = if is_selected {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ACCENT_DIM)
    };
    let name_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    // Two-cell slot so uploaded vs. not stays vertically aligned across rows.
    let (badge, badge_style) = if rec.episode.is_some() {
        ("↑ ", Style::default().fg(OK).add_modifier(Modifier::BOLD))
    } else {
        ("  ", Style::default().fg(ACCENT_DIM))
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
        Span::styled(
            format!("{:>7}  ", rec.duration),
            Style::default().fg(ACCENT_DIM),
        ),
        Span::styled(badge, badge_style),
        Span::styled(label.to_string(), name_style),
    ])
}

fn render_hints(frame: &mut Frame, area: Rect, state: &AppState, accent: Color) {
    // State-specific primary chord first, then a shared `↑↓` (+`o`)
    // block whenever the state accepts recent-list navigation. Routing
    // the `↑↓` slot through `allows_recent_navigation` keeps this row
    // and the matching key handler in `main.rs` in lockstep — adding a
    // new state can flip both at once via a single predicate edit.
    let mut hints: Vec<(&str, &str)> = match state {
        AppState::Idle => vec![("r", " Record  ")],
        AppState::Recording => vec![("s", " Stop & Save  "), ("q", " Stop & Quit")],
        AppState::Processing => vec![],
        AppState::Done(_) => vec![("u", " Upload to LISTEN  ")],
        AppState::Uploading(_) => vec![],
        AppState::Uploaded { .. } => vec![("o", " Open in browser  ")],
        AppState::UploadFailed { .. } => vec![("u", " Retry Upload  ")],
        AppState::ConfirmQuit { .. } => {
            unreachable!("ConfirmQuit is handled by render as a popup")
        }
    };
    if state.allows_recent_navigation() {
        hints.push(("↑↓", " Select  "));
        // Uploaded already advertises `o` in its base row (it's the
        // primary action right after upload), so skip the duplicate.
        if !matches!(state, AppState::Uploaded { .. }) {
            hints.push(("o", " Open in browser  "));
        }
    }
    if matches!(
        state,
        AppState::Done(_) | AppState::Uploaded { .. } | AppState::UploadFailed { .. }
    ) {
        hints.push(("r", " New Recording  "));
    }
    if !matches!(
        state,
        AppState::Recording | AppState::Processing | AppState::Uploading(_)
    ) {
        hints.push(("q", " Quit"));
    }

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
            spans.push(Span::styled(key, key_style));
            spans.push(Span::styled("]", bracket_style));
            spans.push(Span::styled(label, label_style));
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
    use crate::upload::metadata::EpisodeMetadata;

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

    fn rec_with_episode(episode: Option<EpisodeMetadata>) -> RecentRecording {
        RecentRecording {
            path: std::path::PathBuf::from("/tmp/recording_2026.mp3"),
            size: "1 KB".into(),
            duration: "0:01".into(),
            timestamp: "05-01 12:34".into(),
            episode,
        }
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn recent_line_uses_filename_when_no_episode() {
        let rec = rec_with_episode(None);
        let line = format_recent_line(&rec, false);
        let text = line_text(&line);
        assert!(text.contains("recording_2026.mp3"), "got: {text}");
        // No upload badge for un-uploaded rows.
        assert!(!text.contains('↑'), "got: {text}");
    }

    #[test]
    fn recent_line_uses_episode_title_and_badge_when_uploaded() {
        let meta = EpisodeMetadata {
            episode_id: "ep1".into(),
            title: "Cool Episode".into(),
            webview_url: "https://listen.style/p/x/ep1".into(),
            uploaded_at: "2026-05-01T12:34:56+09:00".into(),
        };
        let rec = rec_with_episode(Some(meta));
        let line = format_recent_line(&rec, false);
        let text = line_text(&line);
        assert!(
            text.contains("Cool Episode"),
            "title should replace filename, got: {text}"
        );
        assert!(
            !text.contains("recording_2026.mp3"),
            "filename should not leak through alongside the title, got: {text}"
        );
        assert!(text.contains('↑'), "uploaded badge missing, got: {text}");
    }

    fn spans_text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn sync_indicator_idle_renders_nothing() {
        // Idle = "no sync ever ran"; the user shouldn't see anything that
        // hints there's a separate state machine running.
        assert!(sync_indicator_spans(&SyncState::Idle).is_empty());
    }

    #[test]
    fn sync_indicator_done_renders_nothing() {
        // Done collapses too: the rehydrated titles already speak for
        // themselves in the row list, so a lingering "✓ synced" is noise.
        assert!(sync_indicator_spans(&SyncState::Done { updated: 0 }).is_empty());
        assert!(sync_indicator_spans(&SyncState::Done { updated: 5 }).is_empty());
    }

    #[test]
    fn sync_indicator_syncing_includes_label() {
        let spans = sync_indicator_spans(&SyncState::Syncing);
        let text = spans_text(&spans);
        assert!(text.contains("syncing"), "got: {text}");
    }

    #[test]
    fn sync_indicator_failed_includes_warning_label() {
        // Don't surface the error string — TUI can't show enough to be
        // useful, and a long error would push the count off-screen. A
        // discreet "⚠ sync failed" is enough for the user to know to retry.
        let spans = sync_indicator_spans(&SyncState::Failed("network: timed out".into()));
        let text = spans_text(&spans);
        assert!(text.contains("sync failed"), "got: {text}");
        assert!(
            !text.contains("network"),
            "raw error should not leak into the title bar, got: {text}"
        );
    }
}

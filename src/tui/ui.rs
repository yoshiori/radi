use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use tui_popup::Popup;

use crate::app::{App, AppState};

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
        Line::from(Span::styled(
            "[y] Yes    [n] Cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    let popup = Popup::new(body)
        .title(" Confirm ")
        .style(Style::new().fg(Color::White).bg(Color::DarkGray));
    frame.render_widget(&popup, frame.area());
}

fn render_main(frame: &mut Frame, app: &App, state: &AppState) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // title
        Constraint::Length(1), // device info
        Constraint::Length(5), // status
        Constraint::Length(3), // level meter
        Constraint::Min(1),    // spacer
        Constraint::Length(3), // key hints
    ])
    .split(frame.area());

    // Title
    let title = Paragraph::new("radi - Podcast Recorder")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(title, chunks[0]);

    // Device info
    let device_text = match &app.device_name {
        Some(name) => format!("  🎙 {name}"),
        None => "  🎙 No device".to_string(),
    };
    let device_info = Paragraph::new(device_text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(device_info, chunks[1]);

    // Status
    let (state_text, state_color) = match state {
        AppState::Idle => ("■ Idle", Color::Gray),
        AppState::Recording => ("● Recording", Color::Red),
        AppState::Processing => ("◌ Processing...", Color::Yellow),
        AppState::Uploading(path) => {
            let status = Paragraph::new(vec![
                Line::from(Span::styled(
                    "⇪ Uploading to LISTEN...",
                    Style::default().fg(Color::Cyan),
                )),
                Line::from(""),
                Line::from(format!("  File: {}", path.display())),
            ])
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(status, chunks[2]);

            // The blocking HTTP client gives no byte-level progress, so use a
            // time-based spinner label over a full-width gauge rather than a
            // fake percentage that would look stuck.
            let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let frame_index = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
                / 100) as usize
                % spinner_frames.len();
            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Upload"))
                .gauge_style(Style::default().fg(Color::Cyan))
                .label(format!("{} uploading", spinner_frames[frame_index]))
                .ratio(1.0);
            frame.render_widget(gauge, chunks[3]);

            render_hints(frame, chunks[5], state);
            return;
        }
        AppState::Uploaded { path, webview_url } => {
            let status = Paragraph::new(vec![
                Line::from(Span::styled(
                    "✓ Uploaded",
                    Style::default().fg(Color::Green),
                )),
                Line::from(""),
                Line::from(format!("  URL:  {webview_url}")),
                Line::from(format!("  File: {}", path.display())),
            ])
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(status, chunks[2]);

            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Upload"))
                .gauge_style(Style::default().fg(Color::Green))
                .ratio(1.0);
            frame.render_widget(gauge, chunks[3]);

            render_hints(frame, chunks[5], state);
            return;
        }
        AppState::UploadFailed { path, error } => {
            let status = Paragraph::new(vec![
                Line::from(Span::styled(
                    "✗ Upload failed",
                    Style::default().fg(Color::Red),
                )),
                Line::from(""),
                Line::from(format!("  {error}")),
                Line::from(format!("  File: {}", path.display())),
            ])
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(status, chunks[2]);

            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Upload"))
                .gauge_style(Style::default().fg(Color::Red))
                .ratio(0.0);
            frame.render_widget(gauge, chunks[3]);

            render_hints(frame, chunks[5], state);
            return;
        }
        AppState::ConfirmQuit { previous } => {
            let recording = matches!(previous.as_ref(), AppState::Recording);
            let prompt = if recording {
                "Stop recording and quit? [y/n]"
            } else {
                "Quit radi? [y/n]"
            };
            let status = Paragraph::new(vec![
                Line::from(Span::styled(
                    "? Confirm",
                    Style::default().fg(Color::Yellow),
                )),
                Line::from(""),
                Line::from(format!("  {prompt}")),
            ])
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(status, chunks[2]);

            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Level"))
                .gauge_style(Style::default().fg(Color::Yellow))
                .ratio(0.0);
            frame.render_widget(gauge, chunks[3]);

            render_hints(frame, chunks[5], state);
            return;
        }
        AppState::Done(path) => {
            let text = format!("✓ Done: {}", path.display());
            // Render directly since we need owned string
            let elapsed = format_duration(app.elapsed());
            let status = Paragraph::new(vec![
                Line::from(Span::styled(text, Style::default().fg(Color::Green))),
                Line::from(""),
                Line::from(format!("  Time: {elapsed}")),
                Line::from(format!("  Output: {}", app.output_path.display())),
            ])
            .block(Block::default().borders(Borders::ALL).title("Status"));
            frame.render_widget(status, chunks[2]);

            // Level meter (empty for done state)
            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Level"))
                .gauge_style(Style::default().fg(Color::Green))
                .ratio(0.0);
            frame.render_widget(gauge, chunks[3]);

            // Key hints
            render_hints(frame, chunks[5], state);
            return;
        }
    };

    let elapsed = format_duration(app.elapsed());
    let status = Paragraph::new(vec![
        Line::from(Span::styled(state_text, Style::default().fg(state_color))),
        Line::from(""),
        Line::from(format!("  Time: {elapsed}")),
        Line::from(format!("  Output: {}", app.output_path.display())),
    ])
    .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(status, chunks[2]);

    // Level meter
    let level = app.peak().min(1.0) as f64;
    let gauge_color = if level > 0.8 {
        Color::Red
    } else if level > 0.5 {
        Color::Yellow
    } else {
        Color::Green
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("Level"))
        .gauge_style(Style::default().fg(gauge_color))
        .ratio(level);
    frame.render_widget(gauge, chunks[3]);

    // Key hints
    render_hints(frame, chunks[5], state);
}

fn render_hints(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let hints = match state {
        AppState::Idle => "[r] Record  [q] Quit",
        AppState::Recording => "[s] Stop & Save  [q] Stop & Quit",
        AppState::Processing => "Processing...",
        AppState::Done(_) => "[u] Upload to LISTEN  [r] New Recording  [q] Quit",
        AppState::Uploading(_) => "Uploading...",
        AppState::Uploaded { .. } => "[r] New Recording  [q] Quit",
        AppState::UploadFailed { .. } => "[u] Retry Upload  [r] New Recording  [q] Quit",
        // ConfirmQuit is rendered as an overlay; render_main is only called
        // with the prior state, so this arm is unreachable.
        AppState::ConfirmQuit { .. } => "",
    };
    let paragraph = Paragraph::new(hints)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::TOP));
    frame.render_widget(paragraph, area);
}

fn format_duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

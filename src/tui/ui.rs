use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::app::{App, AppState};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(3), // title
        Constraint::Length(5), // status
        Constraint::Length(3), // level meter
        Constraint::Min(1),    // spacer
        Constraint::Length(3), // key hints
    ])
    .split(frame.area());

    // Title
    let title = Paragraph::new("radi - Podcast Recorder")
        .style(Style::default().fg(Color::Cyan).bold())
        .block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(title, chunks[0]);

    // Status
    let (state_text, state_color) = match &app.state {
        AppState::Idle => ("■ Idle", Color::Gray),
        AppState::Recording => ("● Recording", Color::Red),
        AppState::Processing => ("◌ Processing...", Color::Yellow),
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
            frame.render_widget(status, chunks[1]);

            // Level meter (empty for done state)
            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("Level"))
                .gauge_style(Style::default().fg(Color::Green))
                .ratio(0.0);
            frame.render_widget(gauge, chunks[2]);

            // Key hints
            render_hints(frame, chunks[4], &app.state);
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
    frame.render_widget(status, chunks[1]);

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
    frame.render_widget(gauge, chunks[2]);

    // Key hints
    render_hints(frame, chunks[4], &app.state);
}

fn render_hints(frame: &mut Frame, area: ratatui::layout::Rect, state: &AppState) {
    let hints = match state {
        AppState::Idle => "[R] Record  [Q] Quit",
        AppState::Recording => "[S] Stop & Save  [Q] Stop & Quit",
        AppState::Processing => "Processing...",
        AppState::Done(_) => "[R] New Recording  [Q] Quit",
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

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use tui_big_text::{BigText, PixelSize};

const BANNER: &str = "\
██████╗  █████╗ ██████╗ ██╗
██╔══██╗██╔══██╗██╔══██╗██║
██████╔╝███████║██║  ██║██║
██╔══██╗██╔══██║██║  ██║██║
██║  ██║██║  ██║██████╔╝██║
╚═╝  ╚═╝╚═╝  ╚═╝╚═════╝ ╚═╝";

pub(crate) const BANNER_WIDTH: u16 = 27;
pub(crate) const BANNER_HEIGHT: u16 = 6;

const SUBTITLE: &str = "The TUI podcast recorder";
const TAGLINE: &str = "with ML noise suppression";
const HINT: &str = "press any key…";

const ACCENT_DIM: Color = Color::DarkGray;

pub fn render(frame: &mut Frame, phase: f32) {
    let area = frame.area();
    // Need banner + 1 blank + subtitle + 1 blank + tagline + flex + hint = ~11 rows.
    if area.width >= BANNER_WIDTH + 4 && area.height >= BANNER_HEIGHT + 5 {
        render_full(frame, area, phase);
    } else if area.width >= 20 && area.height >= 8 {
        render_big_text(frame, area, phase);
    } else {
        render_plain(frame, area, phase);
    }
}

fn render_full(frame: &mut Frame, area: Rect, phase: f32) {
    // Content ~11 rows: centre it vertically with a top spacer; bottom spacer
    // eats the slack so the hint lands on the last row.
    let content_height: u16 = BANNER_HEIGHT + 5;
    let top_pad = area.height.saturating_sub(content_height) / 2;

    let rows = Layout::vertical([
        Constraint::Length(top_pad),
        Constraint::Length(BANNER_HEIGHT),
        Constraint::Length(1),
        Constraint::Length(1), // subtitle
        Constraint::Length(1),
        Constraint::Length(1), // tagline
        Constraint::Min(0),
        Constraint::Length(1), // hint
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(Text::from(banner_lines(phase))).alignment(Alignment::Center),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            SUBTITLE,
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            TAGLINE,
            Style::default().fg(ACCENT_DIM),
        )))
        .alignment(Alignment::Center),
        rows[5],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            HINT,
            Style::default().fg(ACCENT_DIM).add_modifier(Modifier::DIM),
        )))
        .alignment(Alignment::Center),
        rows[7],
    );
}

fn render_big_text(frame: &mut Frame, area: Rect, phase: f32) {
    let content_height: u16 = 6;
    let top_pad = area.height.saturating_sub(content_height) / 2;
    let rows = Layout::vertical([
        Constraint::Length(top_pad),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);

    let color = gradient(0.5, phase);
    let big = BigText::builder()
        .pixel_size(PixelSize::HalfHeight)
        .alignment(Alignment::Center)
        .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .lines(vec!["RADI".into()])
        .build();
    frame.render_widget(big, rows[1]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            SUBTITLE,
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center),
        rows[3],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            HINT,
            Style::default().fg(ACCENT_DIM).add_modifier(Modifier::DIM),
        )))
        .alignment(Alignment::Center),
        rows[5],
    );
}

fn render_plain(frame: &mut Frame, area: Rect, phase: f32) {
    let color = gradient(0.3, phase);
    let lines = vec![
        Line::from(Span::styled(
            "RADI",
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(SUBTITLE, Style::default().fg(Color::Gray))),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

/// Render the banner at an interpolated position between the splash dwell
/// location (centre of the frame) and the given `end` rect. Used for the
/// centre-to-header hand-off animation. The banner is always drawn at full
/// size (`BANNER_WIDTH × BANNER_HEIGHT`); only its origin moves.
pub(crate) fn render_floating_banner(frame: &mut Frame, t: f32, end: Rect) {
    let area = frame.area();
    // Must match where splash::render_full places the banner row, so the
    // slide starts exactly where the dwell left it with no visible jump.
    let content_height: u16 = BANNER_HEIGHT + 5;
    let top_pad = area.height.saturating_sub(content_height) / 2;
    let start_x = area.width.saturating_sub(BANNER_WIDTH) / 2;
    let start_y = top_pad;
    let x = lerp_u16(start_x, end.x, t);
    let y = lerp_u16(start_y, end.y, t);
    let rect = Rect::new(x, y, BANNER_WIDTH, BANNER_HEIGHT);
    frame.render_widget(Paragraph::new(Text::from(banner_lines(1.0))), rect);
}

/// Shared banner builder so the splash dwell, the slide hand-off and the
/// permanent header all render identical glyphs & colours.
pub(crate) fn banner_lines(phase: f32) -> Vec<Line<'static>> {
    BANNER.lines().map(|l| banner_line(l, phase)).collect()
}

fn lerp_u16(a: u16, b: u16, t: f32) -> u16 {
    let af = a as f32;
    let bf = b as f32;
    (af + (bf - af) * t).round().clamp(0.0, u16::MAX as f32) as u16
}

/// Build a single banner row with per-column RGB gradient.
/// Spaces are left uncoloured so the gradient reads as applied to the glyphs
/// rather than filling whole rectangles.
fn banner_line(row: &str, phase: f32) -> Line<'static> {
    let total = row.chars().count().max(2) as f32;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(row.chars().count());
    for (i, ch) in row.chars().enumerate() {
        let ratio = i as f32 / (total - 1.0);
        let style = if ch == ' ' {
            Style::default()
        } else {
            Style::default()
                .fg(gradient(ratio, phase))
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(ch.to_string(), style));
    }
    Line::from(spans)
}

/// 3-stop RGB gradient: cyan → magenta → warm orange.
/// `phase ∈ [0, 1]` applies a small left-to-right slide so successive frames
/// of the splash feel animated rather than static.
fn gradient(col_ratio: f32, phase: f32) -> Color {
    const STOPS: [(u8, u8, u8); 3] = [
        (0, 200, 255),   // cyan
        (200, 100, 255), // magenta
        (255, 150, 80),  // warm orange
    ];
    let shift = phase * 0.25 - 0.1;
    let t = (col_ratio + shift).clamp(0.0, 1.0);
    let (a, b, u) = if t <= 0.5 {
        (STOPS[0], STOPS[1], t / 0.5)
    } else {
        (STOPS[1], STOPS[2], (t - 0.5) / 0.5)
    };
    Color::Rgb(
        lerp_u8(a.0, b.0, u),
        lerp_u8(a.1, b.1, u),
        lerp_u8(a.2, b.2, u),
    )
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let af = a as f32;
    let bf = b as f32;
    (af + (bf - af) * t).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_dimensions_match_constants() {
        let lines: Vec<&str> = BANNER.lines().collect();
        assert_eq!(lines.len(), BANNER_HEIGHT as usize);
        for l in &lines {
            assert_eq!(
                l.chars().count(),
                BANNER_WIDTH as usize,
                "line width mismatch: {l:?}"
            );
        }
    }

    #[test]
    fn gradient_starts_cool_ends_warm() {
        let left = gradient(0.0, 1.0);
        let right = gradient(1.0, 1.0);
        match (left, right) {
            (Color::Rgb(lr, _, lb), Color::Rgb(rr, _, rb)) => {
                assert!(lb > lr, "left end should be cool (blue > red): {left:?}");
                assert!(rr > rb, "right end should be warm (red > blue): {right:?}");
            }
            _ => panic!("gradient() must return Color::Rgb"),
        }
    }

    #[test]
    fn lerp_u8_endpoints_and_midpoint() {
        assert_eq!(lerp_u8(0, 200, 0.0), 0);
        assert_eq!(lerp_u8(0, 200, 1.0), 200);
        assert_eq!(lerp_u8(0, 200, 0.5), 100);
    }
}

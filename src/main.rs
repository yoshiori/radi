mod app;
mod audio;
mod config;
mod tui;
mod upload;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Clear;

use app::{App, AppState};
use tui::{event, splash, ui};

fn main() -> anyhow::Result<()> {
    let config = config::Config::load()?;
    let output_dir = config.output_dir_or_default();
    std::fs::create_dir_all(&output_dir).map_err(|e| {
        anyhow::anyhow!(
            "Failed to create output directory {}: {}",
            output_dir.display(),
            e
        )
    })?;

    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, output_dir, config.listen);
    ratatui::restore();
    result
}

fn run_app(
    terminal: &mut ratatui::DefaultTerminal,
    output_dir: PathBuf,
    listen: Option<config::ListenConfig>,
) -> anyhow::Result<()> {
    let mut app = App::new(output_dir.clone());

    // Refresh local sidecars from LISTEN before drawing the first real
    // frame: titles edited on listen.style after upload should show up
    // in Recent on next launch, not stay frozen at the upload-time value.
    if let Some(listen) = listen.clone() {
        app.start_rehydrate(listen);
    }

    run_splash(terminal, &app)?;

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if let Some(key) = event::poll_event(Duration::from_millis(100))? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char(c @ ('y' | 'n'))
                    if matches!(app.state, AppState::ConfirmQuit { .. }) =>
                {
                    if let AppState::ConfirmQuit { previous } =
                        std::mem::replace(&mut app.state, AppState::Idle)
                    {
                        app.state = *previous;
                    }
                    if c == 'y' {
                        if app.state == AppState::Recording {
                            app.stop_recording()?;
                        }
                        app.should_quit = true;
                    }
                }
                _ if matches!(app.state, AppState::ConfirmQuit { .. }) => {}
                KeyCode::Char('r') => match app.state {
                    AppState::Idle => app.start_recording()?,
                    AppState::Done(_)
                    | AppState::Uploaded { .. }
                    | AppState::UploadFailed { .. } => {
                        app = App::new(output_dir.clone());
                        app.start_recording()?;
                    }
                    _ => {}
                },
                KeyCode::Char('s') if app.state == AppState::Recording => {
                    app.stop_recording()?;
                }
                KeyCode::Char('o') => {
                    // Prefer the selected Recent row's webview_url so the
                    // user can re-open *any* uploaded episode, not just the
                    // one that finished uploading this session. Falls back
                    // to the Uploaded state's url so a freshly-uploaded
                    // recording is openable even before the next recent
                    // rescan (750ms throttle) has picked up its sidecar.
                    let url = app
                        .selected_recording()
                        .and_then(|r| r.episode.as_ref())
                        .map(|e| e.webview_url.clone())
                        .or_else(|| match &app.state {
                            AppState::Uploaded { webview_url, .. } => Some(webview_url.clone()),
                            _ => None,
                        });
                    if let Some(url) = url {
                        // Failures here are non-fatal: the URL is still shown
                        // on screen for the user to copy manually.
                        let _ = open::that_detached(&url);
                    }
                }
                KeyCode::Up if app.state.allows_recent_navigation() => {
                    app.select_recent_prev();
                }
                KeyCode::Down if app.state.allows_recent_navigation() => {
                    app.select_recent_next();
                }
                KeyCode::Char('u') => {
                    let path = match &app.state {
                        AppState::Done(p) | AppState::UploadFailed { path: p, .. } => {
                            Some(p.clone())
                        }
                        _ => None,
                    };
                    if let Some(path) = path {
                        match listen.as_ref() {
                            Some(listen) => {
                                app.state = AppState::Done(path);
                                let title = app
                                    .output_path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("recording")
                                    .to_string();
                                app.start_upload(listen, title)?;
                            }
                            None => {
                                app.state = AppState::UploadFailed {
                                    path,
                                    error: "LISTEN not configured: add a [listen] section with podcast_id to ~/.config/radi/config.toml".into(),
                                };
                            }
                        }
                    }
                }
                KeyCode::Char('q') => {
                    let previous = std::mem::replace(&mut app.state, AppState::Idle);
                    app.state = AppState::ConfirmQuit {
                        previous: Box::new(previous),
                    };
                }
                _ => {}
            }
        }

        app.tick();

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Display a brief colourful splash that then slides into the permanent
/// header banner slot before handing off to the main UI.
///
/// Phases:
/// 1. Dwell (0..dwell): centred RADI splash via `splash::render`.
/// 2. Slide (dwell..dwell+slide): main Idle UI is drawn underneath, the
///    header's banner slot is `Clear`ed, and a floating banner is
///    interpolated from the splash position to the header slot with an
///    ease-out cubic. At t=1 the floating banner's rect matches the
///    header's slot exactly, so the transition is seamless.
///
/// The slide phase is skipped when the terminal is too short for the big
/// header (`header_banner_slot` returns `None`) — there's nowhere to land,
/// so dropping straight into the compact main UI is the sensible fallback.
///
/// Skippable with any key at any phase; swallowed keys are intentionally
/// discarded so a user hitting `r`/`q` on the first frame doesn't
/// accidentally trigger recording or quit before the Idle screen is drawn.
fn run_splash(terminal: &mut ratatui::DefaultTerminal, app: &App) -> anyhow::Result<()> {
    // Sampled once to decide whether to schedule a slide phase at all. The
    // actual landing rect is recomputed every frame inside `draw` so a
    // terminal resize during the animation lands the banner in the correct
    // spot for the current layout instead of a stale one.
    let term_size = terminal.size()?;
    let term_rect = Rect::new(0, 0, term_size.width, term_size.height);
    let initial_has_slot = ui::header_banner_slot(term_rect).is_some();

    let dwell = Duration::from_millis(600);
    let slide = Duration::from_millis(600);
    let total = if initial_has_slot {
        dwell + slide
    } else {
        dwell
    };
    let start = Instant::now();

    loop {
        let e = start.elapsed();
        if e >= total {
            break;
        }
        terminal.draw(|frame| {
            if e < dwell {
                let p = (e.as_secs_f32() / dwell.as_secs_f32()).clamp(0.0, 1.0);
                splash::render(frame, p);
            } else if let Some(slot) = ui::header_banner_slot(frame.area()) {
                let raw = ((e - dwell).as_secs_f32() / slide.as_secs_f32()).clamp(0.0, 1.0);
                // Ease-out cubic: fast at the start, decelerating into the slot.
                let t = 1.0 - (1.0 - raw).powi(3);
                ui::render(frame, app);
                frame.render_widget(Clear, slot);
                splash::render_floating_banner(frame, t, slot);
            } else {
                // Window shrank below the big-header threshold mid-slide —
                // just draw the compact UI for the remaining frames.
                ui::render(frame, app);
            }
        })?;
        if let Some(key) = event::poll_event(Duration::from_millis(30))?
            && key.kind == KeyEventKind::Press
        {
            break;
        }
    }
    Ok(())
}

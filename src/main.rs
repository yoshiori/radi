mod app;
mod audio;
mod config;
mod tui;
mod upload;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEventKind};

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

    run_splash(terminal)?;

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
                    if let AppState::Uploaded { webview_url, .. } = &app.state {
                        // Failures here are non-fatal: the URL is still shown
                        // on screen for the user to copy manually.
                        let _ = open::that_detached(webview_url);
                    }
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

/// Display a brief colourful splash before the main UI takes over.
/// Skippable with any key. Keeps swallowed keys from leaking into the main
/// loop so the splash can't accidentally trigger `r`/`q` on the first frame.
fn run_splash(terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
    let start = Instant::now();
    let duration = Duration::from_millis(1200);
    loop {
        let elapsed = start.elapsed();
        if elapsed >= duration {
            break;
        }
        let phase = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
        terminal.draw(|frame| splash::render(frame, phase))?;
        if let Some(key) = event::poll_event(Duration::from_millis(40))?
            && key.kind == KeyEventKind::Press
        {
            break;
        }
    }
    Ok(())
}

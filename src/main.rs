mod app;
mod audio;
mod config;
mod tui;
mod upload;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::KeyCode;

use app::{App, AppState};
use tui::{event, ui};

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

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if let Some(key) = event::poll_event(Duration::from_millis(100))? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match key.code {
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
                KeyCode::Char('u') => {
                    if let Some(listen) = listen.as_ref() {
                        if let AppState::UploadFailed { path, .. } = app.state.clone() {
                            app.state = AppState::Done(path);
                        }
                        if matches!(app.state, AppState::Done(_)) {
                            let title = app
                                .output_path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("recording")
                                .to_string();
                            app.start_upload(listen, title)?;
                        }
                    }
                }
                KeyCode::Char('q') => {
                    if app.state == AppState::Recording {
                        app.stop_recording()?;
                    }
                    app.should_quit = true;
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

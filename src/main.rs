mod app;
mod audio;
mod config;
mod tui;

use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::KeyCode;

use app::{App, AppState};
use tui::{event, ui};

fn main() -> anyhow::Result<()> {
    let config = config::Config::load()?;
    let output_dir = config.output_dir_or_default();
    std::fs::create_dir_all(&output_dir)?;

    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, output_dir);
    ratatui::restore();
    result
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, output_dir: PathBuf) -> anyhow::Result<()> {
    let mut app = App::new(output_dir.clone());

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;

        if let Some(key) = event::poll_event(Duration::from_millis(100))? {
            if key.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('r') => {
                    if app.state == AppState::Idle {
                        app.start_recording()?;
                    } else if matches!(app.state, AppState::Done(_)) {
                        app = App::new(output_dir.clone());
                        app.start_recording()?;
                    }
                }
                KeyCode::Char('s') if app.state == AppState::Recording => {
                    app.stop_recording()?;
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

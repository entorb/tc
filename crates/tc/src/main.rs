mod app;
mod ui;

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tc_core::panel::Panel;

use app::App;

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let open = || Panel::open(&cwd).with_context(|| format!("cannot open {}", cwd.display()));
    let mut app = App::new(open()?, open()?);
    ratatui::run(|terminal| {
        while !app.quit {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            // short poll so worker results show up without a keypress
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                // Windows also reports key releases
                && key.kind == KeyEventKind::Press
            {
                app.on_key(key);
            }
            app.tick();
        }
        Ok(())
    })
}

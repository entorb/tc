mod app;
mod ui;

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
            // Windows also reports key releases
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.on_key(key);
            }
        }
        Ok(())
    })
}

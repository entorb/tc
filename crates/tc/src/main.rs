mod app;
mod ui;

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use tc_core::panel::Panel;

use app::App;

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let open = || Panel::open(&cwd).with_context(|| format!("cannot open {}", cwd.display()));
    let mut app = App::new(open()?, open()?);
    app.hotlist_file = tc_core::hotlist::default_path();
    if let Some(file) = &app.hotlist_file {
        app.hotlist = tc_core::hotlist::load(file).context("cannot read hotlist")?;
    }
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = ratatui::run(|terminal| {
        while !app.quit {
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            // short poll so worker results show up without a keypress
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    // Windows also reports key releases
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(ev) => app.on_mouse(ev),
                    _ => {}
                }
            }
            app.tick();
        }
        Ok(())
    });
    execute!(std::io::stdout(), DisableMouseCapture)?;
    result
}

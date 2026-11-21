mod app;
mod ui;

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use crossterm::clipboard::CopyToClipboard;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
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
    app.history_file = tc_core::cmdline::history_path();
    if let Some(file) = &app.history_file {
        app.history = tc_core::cmdline::load_history(file).context("cannot read history")?;
    }
    execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let result = ratatui::run(|terminal| {
        while !app.quit {
            // shortcut: a save slower than 2 s shows its popup over the panels, not the editor
            let hidden = matches!(&app.mode, app::Mode::Work(j) if !j.visible());
            if !hidden {
                terminal.draw(|frame| ui::draw(frame, &mut app))?;
            }
            // short poll so worker results show up without a keypress
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    // Windows also reports key releases
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(ev) => app.on_mouse(ev),
                    Event::Paste(text) => app.on_paste(text),
                    _ => {}
                }
            }
            // OSC 52: the terminal sets the clipboard, works over SSH
            if let Some(text) = app.copied.take() {
                execute!(std::io::stdout(), CopyToClipboard::to_clipboard_from(text))?;
            }
            if let Some((cmd, dir)) = app.run.take() {
                run_shell(terminal, &cmd, &dir)?;
                app.reload_all();
            }
            app.tick();
        }
        Ok(())
    });
    execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    result
}

/// Give the terminal to the shell command, wait for it and a key, then take it back.
fn run_shell(terminal: &mut DefaultTerminal, cmd: &str, dir: &Path) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    let status = tc_core::cmdline::shell_command(cmd, dir).status();
    match status {
        Ok(s) if s.success() => println!("\n[tc] Press any key"),
        Ok(s) => println!("\n[tc] {s}. Press any key"),
        Err(e) => println!("\n[tc] {cmd}: {e}. Press any key"),
    }
    enable_raw_mode()?;
    while !matches!(event::read()?, Event::Key(k) if k.kind == KeyEventKind::Press) {}
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    terminal.clear()
}

mod ui;

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

use tc_app::app::{self, App};

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let open = || Panel::open(&cwd).with_context(|| format!("cannot open {}", cwd.display()));
    let mut app = App::new(open()?, open()?);
    app.load_config()?;
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
            if let Some((cmd, dir, url)) = app.run.take() {
                let command = match url {
                    Some(url) => tc_remote::ssh_command(&url, &cmd, &tc_core::vfs::rpath(&dir)),
                    None => Ok(tc_core::cmdline::shell_command(&cmd, &dir)),
                };
                run_shell(terminal, &cmd, command)?;
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
    if let Err(e) = app.save_session() {
        eprintln!("tc: {e:#}");
    }
    result
}

/// Give the terminal to the shell command, wait for it and a key, then take it back.
fn run_shell(
    terminal: &mut DefaultTerminal,
    cmd: &str,
    command: tc_core::Result<std::process::Command>,
) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    match command.map(|mut c| c.status()) {
        Ok(Ok(s)) if s.success() => println!("\n[tc] Press any key"),
        Ok(Ok(s)) => println!("\n[tc] {s}. Press any key"),
        Ok(Err(e)) => println!("\n[tc] {cmd}: {e}. Press any key"),
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

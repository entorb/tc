//! Torben's Commander desktop app: the `tc-cli` app logic drawn with egui.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod draw;
mod keys;

use std::fs::File;
use std::io::Write;
use std::process::{Child, Stdio};
use std::time::Duration;

use anyhow::Context;
use eframe::egui::{self, ViewportCommand};
use tc_app::app::{App, Mode};
use tc_app::{KeyModifiers, MouseEvent, MouseEventKind};
use tc_core::panel::Panel;
use tc_core::view::Viewer;

/// Wheel distance (points) of one 3-line scroll step in viewer and editor.
const WHEEL_STEP: f32 = 40.0;

struct Gui {
    app: App,
    look: draw::Look,
    wheel: f32,
    /// Running `:` command: process, its command text and the file collecting its output.
    running: Option<(Child, String, tempfile::NamedTempFile)>,
    /// Output file of the last command, kept while the viewer may show it.
    output: Option<tempfile::NamedTempFile>,
}

/// Text on the system clipboard (egui does not hand it over when clipboard shortcuts are off).
fn system_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let open = || Panel::open(&cwd).with_context(|| format!("cannot open {}", cwd.display()));
    let mut app = App::new(open()?, open()?);
    app.load_config()?;
    app.start_update_check();
    let title = match tc_app::app::VERSION {
        Some(version) => format!("Torben's Commander {version}"),
        None => "Torben's Commander".to_owned(),
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1280.0, 800.0]),
        // Ctrl+C/X/V reach the app as keys: egui swallows Ctrl+V when the clipboard holds no text (files)
        clipboard_shortcuts: false,
        ..Default::default()
    };
    let gui = Gui {
        app,
        look: draw::Look::default(),
        wheel: 0.0,
        running: None,
        output: None,
    };
    eframe::run_native(
        "tc",
        options,
        Box::new(|cc| {
            draw::setup_fonts(&cc.egui_ctx);
            // Ctrl+= / Ctrl+- go to the app, which saves the zoom
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            Ok(Box::new(gui))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.input(&ctx);
        self.app.tick();
        if let Some(text) = self.app.copied.take() {
            ctx.copy_text(text);
        }
        if let Some((cmd, dir, url)) = self.app.run.take() {
            self.start(cmd, &dir, url);
        }
        self.poll_command();
        if ctx.zoom_factor() != self.app.zoom {
            ctx.set_zoom_factor(self.app.zoom);
        }
        draw::draw(ui, &mut self.app, &mut self.look);
        if self.app.quit {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        // worker results (copy progress, find, stats, dir loads) show up without input
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    /// Also runs when the window is closed, not only on F10.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(e) = self.app.save_session() {
            eprintln!("tc: {e:#}");
        }
    }
}

impl Gui {
    fn input(&mut self, ctx: &egui::Context) {
        let (events, alt, wheel) = ctx.input(|i| {
            let alt = i.modifiers.alt && !i.modifiers.ctrl;
            (i.events.clone(), alt, i.smooth_scroll_delta.y)
        });
        // macOS layouts type `[`, `@`, `|` with Option: keep that text, drop the Alt+key
        let typed = cfg!(target_os = "macos") && alt && keys::option_typed(&events);
        for ev in &events {
            if typed && keys::is_alt_char(ev) {
                continue;
            }
            match keys::translate(ev, alt && !typed) {
                // in the file panel Ctrl+V pastes files; elsewhere it pastes text into the prompt or editor
                Some(keys::Input::Key(k))
                    if k.code == tc_app::KeyCode::Char('v')
                        && k.modifiers == KeyModifiers::CONTROL
                        && !self.app.panel_keys() =>
                {
                    if let Some(text) = system_text() {
                        self.app.on_paste(text);
                    }
                }
                Some(keys::Input::Key(k)) => self.app.on_key(k),
                Some(keys::Input::Paste(t)) => self.app.on_paste(t),
                None => {}
            }
        }
        // the panels scroll natively; viewer, editor and processes scroll through the app like the terminal wheel
        if !matches!(
            self.app.mode,
            Mode::View { .. } | Mode::Edit { .. } | Mode::Procs(_)
        ) {
            self.wheel = 0.0;
            return;
        }
        self.wheel += wheel;
        while self.wheel.abs() >= WHEEL_STEP {
            let up = self.wheel > 0.0;
            self.wheel -= WHEEL_STEP.copysign(self.wheel);
            self.app.on_mouse(MouseEvent {
                kind: if up {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                },
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        }
    }

    /// Run a `:` command without a terminal: output goes to a temp file shown in the viewer when done.
    // shortcut: no interactive commands (vim, ssh password prompt), upgrade by spawning a system terminal if wanted
    fn start(&mut self, cmd: String, dir: &std::path::Path, url: Option<String>) {
        let command = match url {
            Some(url) => tc_remote::ssh_command(&url, &cmd, &tc_core::vfs::rpath(dir)),
            None => Ok(tc_core::cmdline::shell_command(&cmd, dir)),
        };
        let spawned = command.map_err(|e| e.to_string()).and_then(|mut c| {
            let out = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            let err = out.reopen().map_err(|e| e.to_string())?;
            let stdout = out.reopen().map_err(|e| e.to_string())?;
            let child = c
                .stdin(Stdio::null())
                .stdout(stdout)
                .stderr(err)
                .spawn()
                .map_err(|e| e.to_string())?;
            Ok((child, out))
        });
        match spawned {
            Ok((child, out)) => {
                self.app.note = format!("running: {cmd}");
                self.running = Some((child, cmd, out));
            }
            Err(e) => self.app.mode = Mode::Message(format!("{cmd}: {e}")),
        }
    }

    fn poll_command(&mut self) {
        let Some((child, cmd, out)) = &mut self.running else {
            return;
        };
        // never replace an open editor, dialog or File Stats: the output waits until the panels show
        if !matches!(self.app.mode, Mode::Normal) {
            return;
        }
        let status = match child.try_wait() {
            Ok(None) => return,
            Ok(Some(s)) if s.success() => "done".to_owned(),
            Ok(Some(s)) => s.to_string(),
            Err(e) => e.to_string(),
        };
        let title = format!("{cmd} [{status}]");
        let appended = File::options()
            .append(true)
            .open(out.path())
            .and_then(|mut f| writeln!(f, "\n[tc] {status}"));
        let view = appended.and_then(|()| Viewer::open(out.path()));
        if let Some((_, _, out)) = self.running.take() {
            self.output = Some(out);
        }
        self.app.reload_all();
        self.app.note.clear();
        self.app.mode = match view {
            Ok(view) => Mode::View { title, view },
            Err(e) => Mode::Message(format!("{title}: {e}")),
        };
    }
}

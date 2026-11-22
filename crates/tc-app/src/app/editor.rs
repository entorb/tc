//! Viewer (F3) and editor (F4): keys, find / replace / goto prompts, load and save.

use super::{App, Done, Mode, edit_key};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::thread;
use tc_core::cmdline::{self, LineEdit};
use tc_core::edit::{self, Editor, Eol, SizeCheck};
use tc_core::vfs::{self, Side};
use tc_core::{
    Error, ops, search,
    view::{Viewer, parse_offset},
};

/// What a text prompt in the viewer or editor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Find,
    Goto,
    Replace,
    /// Second step of replace, `Input::pattern` holds the first answer.
    With,
    /// File panel: select (`+`) or deselect (`-`) files by mask.
    Select,
    Deselect,
}

impl Ask {
    pub fn title(self) -> &'static str {
        match self {
            Ask::Find => "Find (regex)",
            Ask::Goto => "Go to line (N%; offset in hex view)",
            Ask::Replace => "Replace (regex)",
            Ask::With => "Replace with ($1 = group)",
            Ask::Select => "Select files (mask, default *)",
            Ask::Deselect => "Deselect files (mask, default *)",
        }
    }
}

pub struct Input {
    pub ask: Ask,
    pub text: LineEdit,
    pub(super) pattern: String,
    /// Replace asks for each match instead of replacing all.
    pub(super) each: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The file changed on disk since it was opened.
    Changed,
    Encoding,
    Eol,
}

impl Mode {
    /// The editor shown behind prompts.
    pub fn editor_mut(&mut self) -> Option<&mut Editor> {
        match self {
            Mode::Edit { editor, .. } | Mode::AskSave { editor, .. } | Mode::AskExit(editor) => {
                Some(editor)
            }
            _ => None,
        }
    }
}

impl App {
    pub(super) fn ask_viewer_key(
        &mut self,
        path: PathBuf,
        text: String,
        can_load: bool,
        code: KeyCode,
    ) {
        match code {
            KeyCode::Char('y' | 'Y') => self.open_viewer(path),
            KeyCode::Char('n' | 'N') if can_load => self.spawn_load(path, None),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {}
            _ => {
                self.mode = Mode::AskViewer {
                    path,
                    text,
                    can_load,
                }
            }
        }
    }

    pub(super) fn editor_key(&mut self, mut editor: Editor, error: Option<String>, key: KeyEvent) {
        if error.is_some() {
            self.mode = Mode::Edit {
                editor,
                error: None,
            };
            return;
        }
        let page = self.page as isize;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.replacing.is_some() {
            self.mode = Mode::Edit {
                editor,
                error: None,
            };
            match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.replace_step(true),
                KeyCode::Char('n' | 'N' | ' ') => self.replace_step(false),
                KeyCode::Char('a' | 'A') => {
                    while self.replacing.is_some() {
                        self.replace_step(true);
                    }
                }
                _ => self.replacing = None,
            }
            return;
        }
        let save =
            key.code == KeyCode::F(2) || (ctrl && matches!(key.code, KeyCode::Char('s' | 'S')));
        let prompt = matches!(key.code, KeyCode::F(3 | 4 | 7))
            || (ctrl && matches!(key.code, KeyCode::Char('g' | 'f' | 'r')));
        if matches!(
            key.code,
            KeyCode::Up
                | KeyCode::Down
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        ) {
            editor.select(shift);
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => {
                if editor.dirty() {
                    self.mode = Mode::AskExit(editor);
                } else {
                    self.leave_editor();
                }
                return;
            }
            _ if save => {
                editor.format_markdown();
                self.next_save_prompt(editor, false, false, Step::Encoding);
                return;
            }
            KeyCode::F(5) => self.format_markdown(&mut editor),
            _ if prompt => {
                self.mode = Mode::Edit {
                    editor,
                    error: None,
                };
                match key.code {
                    KeyCode::F(3) => self.find_or_ask(!shift),
                    KeyCode::F(7) if shift => self.find_or_ask(true),
                    KeyCode::F(4) | KeyCode::Char('r') => self.ask(Ask::Replace),
                    KeyCode::F(7) | KeyCode::Char('f') => self.ask(Ask::Find),
                    _ => self.ask(Ask::Goto),
                }
                return;
            }
            KeyCode::Up => editor.move_v(-1),
            KeyCode::Down => editor.move_v(1),
            KeyCode::PageUp => editor.move_v(-page),
            KeyCode::PageDown => editor.move_v(page),
            KeyCode::Char('w') if alt => editor.wrap = !editor.wrap,
            KeyCode::Char('l') if alt => editor.numbers = !editor.numbers,
            KeyCode::Char('c') if alt => self.toggle_case(),
            KeyCode::Char('r') if alt => {
                self.mode = Mode::Edit {
                    editor,
                    error: None,
                };
                self.ask(Ask::Replace);
                if let Some(i) = &mut self.input {
                    i.each = true;
                }
                return;
            }
            KeyCode::Left if ctrl => editor.move_word(false),
            KeyCode::Right if ctrl => editor.move_word(true),
            KeyCode::Home if ctrl => editor.goto_idx(0),
            KeyCode::End if ctrl => editor.goto_idx(usize::MAX),
            KeyCode::Left => editor.move_h(-1),
            KeyCode::Right => editor.move_h(1),
            KeyCode::Home => editor.home(),
            KeyCode::End => editor.end(),
            KeyCode::Enter => editor.newline(),
            KeyCode::BackTab => editor.unindent(),
            KeyCode::Tab if shift => editor.unindent(),
            KeyCode::Tab => editor.insert_char('\t'),
            KeyCode::Backspace if ctrl => editor.delete_word(false),
            KeyCode::Delete if ctrl => editor.delete_word(true),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::F(8) => editor.delete_line(),
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                'z' => editor.undo(),
                'y' => editor.redo(),
                'c' | 'x' => {
                    let text = if c.eq_ignore_ascii_case(&'c') {
                        editor.copy()
                    } else {
                        editor.cut()
                    };
                    if let Some(text) = text {
                        self.clipboard.clone_from(&text);
                        self.copied = Some(text);
                    }
                }
                'v' => editor.insert(&self.clipboard, false),
                'k' => editor.delete_line(),
                'a' => editor.select_all(),
                'd' => editor.duplicate_line(),
                _ => {}
            },
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => editor.insert_char(c),
            _ => {}
        }
        self.mode = Mode::Edit {
            editor,
            error: None,
        };
    }

    /// F5 in the editor: blank lines around Markdown blocks (one undo step).
    pub(super) fn format_markdown(&mut self, editor: &mut Editor) {
        if !editor.format_markdown() {
            self.note = "format: Markdown files, nothing to change".into();
        }
    }

    /// Alt+C in the viewer and editor: searches match case exactly, or ignore it without capitals.
    pub(super) fn toggle_case(&mut self) {
        self.exact = !self.exact;
        self.note = if self.exact {
            "case sensitive"
        } else {
            "smart case"
        }
        .into();
    }

    /// F4 in the viewer: the editor on the same local file, at the top line.
    pub(super) fn view_to_editor(&mut self, title: String, view: Viewer) {
        if self.view_tmp.is_some() || self.edit_remote.is_some() {
            self.note = "edit: local files only".into();
            return self.mode = Mode::View { title, view };
        }
        let line = view.top_line().map(|n| n + 1);
        self.edit_path_at(view.path().to_owned(), line);
    }

    // Alt+W, Alt+L, Ctrl+F and Ctrl+G act like their plain letters: modifiers are not checked
    pub(super) fn viewer_key(&mut self, title: String, mut view: Viewer, key: KeyEvent) {
        let page = self.page as isize;
        let result = match key.code {
            KeyCode::Esc | KeyCode::F(3 | 10) | KeyCode::Char('q') => return,
            KeyCode::F(4) => return self.view_to_editor(title, view),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.toggle_case();
                Ok(())
            }
            KeyCode::Up => view.scroll(-1),
            KeyCode::Down => view.scroll(1),
            KeyCode::PageUp => view.scroll(-page),
            KeyCode::PageDown => view.scroll(page),
            KeyCode::Home => {
                view.home();
                Ok(())
            }
            KeyCode::End => view.end(self.page),
            KeyCode::Left | KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                return self.view_step(title, view, key.code == KeyCode::Right);
            }
            // macOS terminals send Option+Left/Right as ESC b / ESC f
            KeyCode::Char('b' | 'f') if key.modifiers.contains(KeyModifiers::ALT) => {
                return self.view_step(title, view, key.code == KeyCode::Char('f'));
            }
            KeyCode::Left | KeyCode::Right => {
                view.scroll_h(if key.code == KeyCode::Left { -8 } else { 8 });
                Ok(())
            }
            KeyCode::Char('h') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                view.set_hex(!view.hex())
            }
            KeyCode::Char('w') => {
                view.wrap = !view.wrap;
                Ok(())
            }
            KeyCode::Char('l') => {
                self.view_numbers = !self.view_numbers;
                Ok(())
            }
            KeyCode::Char('/' | 'f' | 'n' | 'N' | 'g') | KeyCode::F(7) => {
                self.mode = Mode::View { title, view };
                match key.code {
                    KeyCode::Char('n') => self.find_or_ask(true),
                    KeyCode::Char('N') => self.find_or_ask(false),
                    KeyCode::Char('g') => self.ask(Ask::Goto),
                    KeyCode::F(7) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        self.find_or_ask(true)
                    }
                    _ => self.ask(Ask::Find),
                }
                return;
            }
            _ => Ok(()),
        };
        self.mode = match result {
            Ok(()) => Mode::View { title, view },
            Err(e) => Mode::Message(format!("{title}: {e}")),
        };
    }

    /// Viewer (top line) or editor (cursor) at `line` when known; media files open the
    /// host's preview (F3) or default application (F4).
    pub(super) fn open_at(&mut self, path: PathBuf, line: Option<u64>, edit: bool) {
        match (edit, cmdline::opens_externally(&path)) {
            (false, true) => self.open_preview(path),
            (true, true) => self.open_default(path),
            (true, false) => self.edit_path_at(path, line),
            (false, false) => {
                self.open_viewer(path);
                if let (Some(n), Mode::View { view, .. }) = (line, &mut self.mode)
                    && let Err(e) = view.goto_line(n)
                {
                    self.mode = Mode::Message(e.to_string());
                }
            }
        }
    }

    pub(super) fn ask(&mut self, ask: Ask) {
        self.input = Some(Input {
            ask,
            text: LineEdit::default(),
            pattern: String::new(),
            each: false,
            error: None,
        });
    }

    pub(super) fn find_or_ask(&mut self, forward: bool) {
        if self.search.is_some() {
            self.find(forward);
        } else {
            self.ask(Ask::Find);
        }
    }

    /// Select or show the next match of the last search in the viewer or editor.
    pub(super) fn find(&mut self, forward: bool) {
        let Some(re) = &self.search else {
            return;
        };
        let found = match &mut self.mode {
            Mode::Edit { editor, .. } => Ok(editor.find(re, forward, true)),
            Mode::View { view, .. } => view.search(re, forward),
            _ => return,
        };
        match found {
            Ok(true) => {}
            Ok(false) => self.note = "not found".into(),
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    /// One-by-one replace: optionally replaces the selected match, then selects the next one
    /// (to the end of the file, no wrap).
    pub(super) fn replace_step(&mut self, replace: bool) {
        let (Some(re), Some(with), Mode::Edit { editor, .. }) =
            (&self.search, &self.replacing, &mut self.mode)
        else {
            self.replacing = None;
            return;
        };
        if replace {
            editor.replace_selection(re, with);
        }
        let more = editor.find(re, true, false);
        self.note = if more {
            "Replace?  [y] Yes   [n] Skip   [a] All   [Esc] Cancel".into()
        } else {
            self.replacing = None;
            "replace done".into()
        };
    }

    /// Alt+Right / Alt+Left: view the next / previous file of the panel, cursor follows.
    pub(super) fn view_step(&mut self, title: String, view: Viewer, forward: bool) {
        let dir = if forward { "next" } else { "previous" };
        let from_panel = self.found.is_none() && self.stats.is_none() && self.sync_back.is_none();
        let next = if from_panel && self.current_file().as_deref() == Some(view.path()) {
            self.next_viewable(forward)
        } else {
            self.note = "next / previous file: panel files only".into();
            self.mode = Mode::View { title, view };
            return;
        };
        let Some(i) = next else {
            self.note = format!("no {dir} file");
            self.mode = Mode::View { title, view };
            return;
        };
        self.panels[self.active].set_cursor(i);
        let Some(path) = self.current_file() else {
            return;
        };
        let line = self.panels[self.active].hit_line(path.as_os_str());
        self.open_at(path, line, false);
        if let Mode::View { view: v, .. } = &mut self.mode {
            v.wrap = view.wrap;
            if view.hex()
                && let Err(e) = v.set_hex(true)
            {
                self.mode = Mode::Message(e.to_string());
            }
        }
    }

    /// Index of the nearest file before or after the cursor that the viewer can show.
    pub(super) fn next_viewable(&self, forward: bool) -> Option<usize> {
        let p = &self.panels[self.active];
        let cur = p.cursor();
        let ok = |&i: &usize| {
            let e = &p.entries()[i];
            !e.is_dir && !cmdline::opens_externally(&p.path().join(&e.name))
        };
        if forward {
            (cur + 1..p.entries().len()).find(ok)
        } else {
            (0..cur).rev().find(ok)
        }
    }

    pub(super) fn goto(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim();
        let err = |_| "not a number".to_owned();
        match &mut self.mode {
            Mode::Edit { editor, .. } => editor.goto_line(text.parse().map_err(err)?),
            Mode::View { view, .. } => match text.strip_suffix('%') {
                Some(p) => view.goto_percent(p.trim().parse().map_err(err)?),
                None if view.hex() => view.goto_offset(parse_offset(text).ok_or("not an offset")?),
                None => view.goto_line(text.parse().map_err(err)?),
            }
            .map_err(|e| e.to_string())?,
            _ => {}
        }
        Ok(())
    }

    /// Key in the open prompt; Enter runs it, errors keep it open.
    pub(super) fn input_key(&mut self, key: KeyEvent) {
        let Some(mut input) = self.input.take() else {
            return;
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Up if matches!(input.ask, Ask::Find | Ask::Replace) => {
                input.text.prev(&self.find_history);
            }
            KeyCode::Down if matches!(input.ask, Ask::Find | Ask::Replace) => {
                input.text.next(&self.find_history);
            }
            KeyCode::Enter => match self.submit(&mut input) {
                Ok(true) => return,
                Ok(false) => {}
                Err(e) => input.error = Some(e),
            },
            _ => {
                if edit_key(&mut input.text, key) {
                    input.error = None;
                }
            }
        }
        self.input = Some(input);
    }

    /// `Ok(true)` when done, `Ok(false)` when the prompt goes on with its next step.
    pub(super) fn submit(&mut self, input: &mut Input) -> Result<bool, String> {
        let text = input.text.text.clone();
        let exact = self.exact;
        let compile = |p: &str| search::compile_case(p, exact).map_err(|e| e.to_string());
        match input.ask {
            Ask::Goto => self.goto(&text)?,
            Ask::Select | Ask::Deselect => {
                let on = input.ask == Ask::Select;
                self.panels[self.active].select_mask(&text, on);
            }
            Ask::Find => {
                // empty answer repeats the last search
                if !text.is_empty() {
                    self.search = Some(compile(&text)?);
                    self.note = self.remember("find-history", &text);
                } else if self.search.is_none() {
                    return Err("empty pattern".into());
                }
                self.find(true);
            }
            Ask::Replace => {
                compile(&text)?;
                self.note = self.remember("find-history", &text);
                input.pattern = text;
                input.text.clear();
                input.ask = Ask::With;
                return Ok(false);
            }
            Ask::With => {
                let re = compile(&input.pattern)?;
                if input.each {
                    if re.is_match("") {
                        return Err("pattern matches empty text".into());
                    }
                    self.search = Some(re);
                    self.replacing = Some(text);
                    self.replace_step(false);
                } else {
                    if let Mode::Edit { editor, .. } = &mut self.mode {
                        self.note = format!("{} replaced", editor.replace_all(&re, &text));
                    }
                    self.search = Some(re);
                }
            }
        }
        Ok(true)
    }

    #[cfg(test)]
    pub(super) fn mode_view_percent(&self) -> u64 {
        match &self.mode {
            Mode::View { view, .. } => view.percent(),
            _ => panic!("not in viewer"),
        }
    }

    /// Start `cmd path` detached; a missing command asks for another one.
    // shortcut: no terminal suspend, so console editors (vim) do not work, upgrade if wanted
    pub(super) fn external_edit(&mut self, path: PathBuf, cmd: String) {
        use std::process::{Command, Stdio};
        let cmd = cmd.trim().to_string();
        let spawned = Command::new(&cmd)
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            // reap it when it exits, else it stays a zombie until tc quits
            Ok(mut child) => drop(thread::spawn(move || child.wait())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.mode = Mode::Editor(path, LineEdit::default())
            }
            Err(e) => self.mode = Mode::Message(format!("{cmd}: {e}")),
        }
    }

    pub(super) fn open_viewer(&mut self, path: PathBuf) {
        self.mode = match Viewer::open(&path) {
            Ok(view) => Mode::View {
                title: path.display().to_string(),
                view,
            },
            Err(e) => Mode::Message(format!("{}: {e}", path.display())),
        };
    }

    pub(super) fn edit_path(&mut self, path: PathBuf) {
        self.edit_path_at(path, None);
    }

    /// Editor with the cursor on `line` (1-based) when given.
    pub(super) fn edit_path_at(&mut self, path: PathBuf, line: Option<u64>) {
        let ask = |text: String, can_load| Mode::AskViewer {
            path: path.clone(),
            text,
            can_load,
        };
        match edit::check_size(&path) {
            Ok(SizeCheck::Ok) => self.spawn_load(path.clone(), line),
            Ok(SizeCheck::Large(n)) => {
                self.mode = ask(
                    format!(
                        "File is {}, open in viewer instead?\n\n[y] Viewer   [n] Edit   [Esc] Cancel",
                        edit::human(n)
                    ),
                    true,
                )
            }
            Err(e @ Error::OutOfMemory { .. }) => {
                self.mode = ask(
                    format!("{e}. Open in viewer?\n\n[y] Viewer   [Esc] Cancel"),
                    false,
                )
            }
            Err(e) => self.mode = Mode::Message(e.to_string()),
        }
    }

    pub(super) fn spawn_load(&mut self, path: PathBuf, line: Option<u64>) {
        let total = std::fs::metadata(&path).map_or(1, |m| m.len());
        let p = path.clone();
        self.spawn("Loading", path, total, false, move |progress, cancel| {
            let mut loaded = Editor::open(&p, progress, cancel);
            if let (Ok(e), Some(n)) = (&mut loaded, line) {
                e.goto_line(usize::try_from(n).unwrap_or(usize::MAX));
            }
            Done::Loaded(loaded)
        });
    }

    pub(super) fn spawn_save(
        &mut self,
        mut editor: Editor,
        to_utf8: bool,
        to_lf: bool,
        exit: bool,
    ) {
        let (path, total) = (editor.path().to_owned(), editor.line_count() as u64);
        let upload = self
            .edit_remote
            .as_ref()
            .map(|(r, p, _)| (r.clone(), p.clone()));
        self.spawn("Saving", path, total, exit, move |progress, cancel| {
            let mut result = editor.save(to_utf8, to_lf, progress, cancel);
            if let (Ok(()), Some((r, target))) = (&result, upload) {
                // replaces the remote file via temp name + rename, like an overwrite in F5
                let p = ops::Progress::default();
                let mut over = |_: &Path, _: &Path| ops::Decision::Overwrite;
                let (src, dst) = (Side::Local, Side::Remote(r));
                result = vfs::transfer(&src, editor.path(), &dst, &target, false, &mut over, &p);
            }
            Done::Saved(editor, result)
        });
    }

    /// Ask about encoding, then line ends, only where the file differs from UTF-8/LF.
    pub(super) fn next_save_prompt(
        &mut self,
        editor: Editor,
        exit: bool,
        to_utf8: bool,
        from: Step,
    ) {
        if editor.changed_on_disk() {
            self.mode = Mode::AskSave {
                editor,
                to_utf8,
                exit,
                step: Step::Changed,
            };
        } else {
            self.save_prompts(editor, exit, to_utf8, from);
        }
    }

    pub(super) fn save_prompts(&mut self, editor: Editor, exit: bool, to_utf8: bool, from: Step) {
        let step = if from == Step::Encoding && !editor.is_utf8() {
            Step::Encoding
        } else if editor.eol() != Eol::Lf {
            Step::Eol
        } else {
            return self.spawn_save(editor, to_utf8, false, exit);
        };
        self.mode = Mode::AskSave {
            editor,
            to_utf8,
            exit,
            step,
        };
    }

    pub(super) fn leave_editor(&mut self) {
        self.mode = Mode::Normal;
        self.finish(None);
    }
}

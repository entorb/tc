//! Viewer (F3) and editor (F4): keys, find / replace / goto prompts, load and save.

use super::widgets::step_key;
use super::{App, Done, FormRow, Mode, edit_key};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::thread;
use std::time::{Duration, Instant};
use tc_core::cmdline::{self, LineEdit};
use tc_core::edit::{self, Editor, Eol, SizeCheck};
use tc_core::vfs::{self, Side};
use tc_core::{
    Error, ops, search,
    view::{Viewer, parse_offset},
};

/// Files up to this size are saved and reloaded on the UI thread: no worker, no panel flash.
const INLINE_BYTES: u64 = 4 << 20;

/// What a text prompt in the viewer or editor asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Goto,
    /// File panel: select (`+`) or deselect (`-`) files by mask.
    Select,
    Deselect,
}

impl Ask {
    pub fn title(self) -> &'static str {
        match self {
            Ask::Goto => "Go to line (N%; offset in hex view)",
            Ask::Select => "Select files (mask, default *)",
            Ask::Deselect => "Deselect files (mask, default *)",
        }
    }
}

pub struct Input {
    pub ask: Ask,
    pub text: LineEdit,
    pub error: Option<String>,
}

/// Search dialog of the viewer (Find) or editor (Find, Replace with, options), over the text.
pub struct SearchForm {
    pub rows: Vec<FormRow>,
    pub focus: usize,
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

/// Alt+Up/Down move the cursor line or the selected lines; with Shift copy the line (VS Code).
fn line_key(editor: &mut Editor, down: bool, shift: bool) {
    if !shift {
        return editor.move_lines(down);
    }
    editor.select(false);
    editor.duplicate_line();
    if !down {
        editor.move_v(-1);
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
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => self.open_viewer(path),
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
        let prompt = matches!(key.code, KeyCode::F(3 | 7))
            || (ctrl && matches!(key.code, KeyCode::Char('g' | 'f' | 'r')));
        // Alt+Up/Down move lines (VS Code), they keep the selection
        let line_move = alt && matches!(key.code, KeyCode::Up | KeyCode::Down);
        if !line_move
            && matches!(
                key.code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        {
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
                // shown right away, not only after a reopen
                editor.tidy();
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
                    KeyCode::F(3) if shift => self.find_or_ask(false),
                    KeyCode::F(7) if shift => self.find_or_ask(true),
                    KeyCode::Char('g') => self.ask(Ask::Goto),
                    KeyCode::Char('r') => self.open_search(true, false),
                    _ => self.open_search(false, false),
                }
                return;
            }
            KeyCode::Up | KeyCode::Down if line_move => {
                line_key(&mut editor, key.code == KeyCode::Down, shift)
            }
            KeyCode::Enter if ctrl => editor.open_line(!shift),
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
                self.open_search(true, true);
                return;
            }
            // Alt: macOS Option+arrow, Ctrl+arrow switches Spaces there
            KeyCode::Left if ctrl || alt => editor.move_word(false),
            KeyCode::Right if ctrl || alt => editor.move_word(true),
            KeyCode::Home if ctrl => editor.goto_idx(0),
            KeyCode::End if ctrl => editor.goto_idx(usize::MAX),
            KeyCode::Left => editor.move_h(-1),
            KeyCode::Right => editor.move_h(1),
            KeyCode::Home => editor.home(),
            KeyCode::End => editor.end(),
            KeyCode::Enter => editor.newline(),
            KeyCode::BackTab => editor.unindent(),
            KeyCode::Tab => editor.insert_char('\t'),
            KeyCode::Backspace if ctrl || alt => editor.delete_word(false),
            KeyCode::Delete if ctrl || alt => editor.delete_word(true),
            KeyCode::Backspace => editor.backspace(),
            KeyCode::Delete => editor.delete(),
            KeyCode::F(8) => editor.delete_line(),
            KeyCode::Char(c) if ctrl => self.editor_ctrl(&mut editor, c),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => editor.insert_char(c),
            _ => {}
        }
        self.mode = Mode::Edit {
            editor,
            error: None,
        };
    }

    /// Ctrl+letter in the editor.
    fn editor_ctrl(&mut self, editor: &mut Editor, c: char) {
        match c.to_ascii_lowercase() {
            'z' => editor.undo(),
            'y' => editor.redo(),
            'c' | 'x' => self.editor_copy(editor, c.eq_ignore_ascii_case(&'x')),
            'v' if self.line_clip => editor.paste_lines(&self.clipboard),
            'v' => editor.insert(&self.clipboard, false),
            'l' => editor.select_line(),
            'k' => editor.delete_line(),
            'a' => editor.select_all(),
            'd' => editor.duplicate_line(),
            _ => {}
        }
    }

    /// Ctrl+C / Ctrl+X: the selection, else the whole cursor line (VS Code).
    fn editor_copy(&mut self, editor: &mut Editor, cut: bool) {
        let selected = editor.copy();
        self.line_clip = selected.is_none();
        let text = match selected {
            Some(t) if cut => {
                editor.cut();
                t
            }
            Some(t) => t,
            None => {
                let line = editor.line_copy();
                if cut {
                    editor.delete_line();
                }
                line
            }
        };
        self.clipboard.clone_from(&text);
        self.copied = Some(text);
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
                    _ => self.open_search(false, false),
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
            error: None,
        });
    }

    pub(super) fn find_or_ask(&mut self, forward: bool) {
        if self.search.is_some() {
            self.find(forward);
        } else {
            self.open_search(false, false);
        }
    }

    /// Opens the search dialog; `replace` ticks Replace (editor only), `each` Confirm each.
    pub(super) fn open_search(&mut self, replace: bool, each: bool) {
        let editable = matches!(self.mode, Mode::Edit { .. });
        let mut rows = vec![FormRow::Text("Find", LineEdit::default())];
        if editable {
            rows.push(FormRow::Text("Replace with", LineEdit::default()));
            rows.push(FormRow::Check("Replace", replace));
            rows.push(FormRow::Check("Confirm each", each));
        }
        rows.push(FormRow::Check("Regex", self.search_regex));
        rows.push(FormRow::Check("Case sensitive", self.exact));
        self.search_form = Some(SearchForm {
            rows,
            focus: 0,
            error: None,
        });
    }

    /// Keys in the search dialog: Tab / Up / Down move between rows, Space toggles, Enter runs.
    pub(super) fn search_dialog_key(&mut self, key: KeyEvent) {
        let Some(form) = &mut self.search_form else {
            return;
        };
        form.error = None;
        match key.code {
            KeyCode::Esc | KeyCode::F(10) => self.search_form = None,
            KeyCode::Enter => self.search_enter(),
            KeyCode::Up | KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                if let FormRow::Text(_, l) = &mut form.rows[0] {
                    match key.code {
                        KeyCode::Up => l.prev(&self.find_history),
                        _ => l.next(&self.find_history),
                    }
                }
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up => {
                form.focus = step_key(key.code, form.focus, form.rows.len());
            }
            _ => form.rows[form.focus].key(key),
        }
    }

    /// Enter in the search dialog: runs it and closes; an error keeps the dialog open.
    pub(super) fn search_enter(&mut self) {
        let Some(form) = self.search_form.take() else {
            return;
        };
        if let Err(e) = self.run_search(&form.rows) {
            self.search_form = Some(SearchForm {
                error: Some(e),
                ..form
            });
        }
    }

    /// Finds the first match, or replaces in the editor (all at once, or one by one).
    fn run_search(&mut self, rows: &[FormRow]) -> Result<(), String> {
        let text = row_text(rows, "Find");
        let (regex, exact) = (row_on(rows, "Regex"), row_on(rows, "Case sensitive"));
        let re = if text.is_empty() {
            // empty answer repeats the last search
            self.search.clone().ok_or("empty pattern")?
        } else {
            let pattern = if regex {
                text.clone()
            } else {
                regex::escape(&text)
            };
            search::compile_case(&pattern, exact).map_err(|e| e.to_string())?
        };
        self.exact = exact;
        self.search_regex = regex;
        if !text.is_empty() {
            self.note = self.remember("find-history", &text);
        }
        self.search = Some(re.clone());
        if !row_on(rows, "Replace") {
            self.find(true);
            return Ok(());
        }
        let each = row_on(rows, "Confirm each");
        let with = row_text(rows, "Replace with");
        // literal text: a `$` is not a group reference
        let with = if regex { with } else { with.replace('$', "$$") };
        if each {
            if re.is_match("") {
                return Err("pattern matches empty text".into());
            }
            self.replacing = Some(with);
            self.replace_step(false);
        } else if let Mode::Edit { editor, .. } = &mut self.mode {
            self.note = format!("{} replaced", editor.replace_all(&re, &with));
        }
        Ok(())
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
        let from_panel = self.found.is_none() && self.stats.is_none();
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
            KeyCode::Enter => match self.submit(&input) {
                Ok(()) => return,
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

    /// Runs the prompt's answer; an error keeps the prompt open.
    pub(super) fn submit(&mut self, input: &Input) -> Result<(), String> {
        let text = &input.text.text;
        match input.ask {
            Ask::Goto => self.goto(text)?,
            Ask::Select | Ask::Deselect => {
                let on = input.ask == Ask::Select;
                self.panels[self.active].select_mask(text, on);
            }
        }
        Ok(())
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
                        "File is {}, open in viewer instead?\n\n[y/Enter] Viewer   [n] Edit   [Esc] Cancel",
                        edit::human(n)
                    ),
                    true,
                )
            }
            Err(e @ Error::OutOfMemory { .. }) => {
                self.mode = ask(
                    format!("{e}. Open in viewer?\n\n[y/Enter] Viewer   [Esc] Cancel"),
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
        // a worker hides the editor until it is done: the panels flash for a moment
        // shortcut: saved inline up to INLINE_BYTES on disk, bigger or remote files keep the worker
        let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= INLINE_BYTES);
        if upload.is_none() && small {
            let result = editor.save(to_utf8, to_lf, &AtomicU64::new(0), &AtomicBool::new(false));
            return self.saved(editor, result, exit);
        }
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

    /// Once a second: a panel directory or a viewed file that changed on disk is read again, an
    /// edited file too unless it has unsaved edits; then the editor asks (once per change).
    pub(super) fn watch_tick(&mut self) {
        if self.watch_at.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.watch_at = Instant::now();
        match &mut self.mode {
            Mode::Normal => {
                for p in &mut self.panels {
                    if p.changed_on_disk()
                        && let Err(e) = p.reload()
                    {
                        self.note = e.to_string();
                    }
                }
            }
            Mode::View { view, .. } if view.changed_on_disk() => {
                self.note = match view.reload() {
                    Ok(()) => "reloaded: the file changed on disk".into(),
                    Err(e) => format!("reload failed: {e}"),
                };
            }
            Mode::Edit {
                editor,
                error: None,
            } if editor.changed_on_disk() && editor.disk_mtime() != self.watch_seen => {
                self.watch_seen = editor.disk_mtime();
                let Mode::Edit { editor, .. } = std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    return;
                };
                if editor.dirty() {
                    self.mode = Mode::AskSave {
                        editor,
                        to_utf8: false,
                        exit: false,
                        step: Step::Changed,
                    };
                } else {
                    self.reload_editor(editor);
                    self.note = "reloaded: the file changed on disk".into();
                }
            }
            _ => {}
        }
    }

    /// Read the edited file again, edits dropped; cursor line and view settings stay.
    pub(super) fn reload_editor(&mut self, editor: Editor) {
        let (path, line) = (editor.path().to_owned(), editor.cursor.0 + 1);
        if !std::fs::metadata(&path).is_ok_and(|m| m.len() <= INLINE_BYTES) {
            return self.spawn_load(path, Some(line as u64));
        }
        self.mode = match Editor::open(&path, &AtomicU64::new(0), &AtomicBool::new(false)) {
            Ok(mut fresh) => {
                fresh.goto_line(line);
                (fresh.wrap, fresh.numbers) = (editor.wrap, editor.numbers);
                Mode::Edit {
                    editor: fresh,
                    error: None,
                }
            }
            Err(e) => Mode::Edit {
                editor,
                error: Some(e.to_string()),
            },
        };
    }

    /// Back in the editor after a save (with its error), or out of it when saved on exit.
    pub(super) fn saved(&mut self, editor: Editor, result: tc_core::Result<()>, exit: bool) {
        if result.is_ok() && exit {
            return self.leave_editor();
        }
        let error = result.err().filter(|e| !matches!(e, Error::Cancelled));
        self.mode = Mode::Edit {
            editor,
            error: error.map(|e| e.to_string()),
        };
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

/// Text of the search dialog row `label` (empty when the row is not there).
fn row_text(rows: &[FormRow], label: &str) -> String {
    rows.iter()
        .find_map(|r| match r {
            FormRow::Text(l, e) if *l == label => Some(e.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Whether the checkbox `label` of the search dialog is on (false when the row is not there).
fn row_on(rows: &[FormRow], label: &str) -> bool {
    rows.iter()
        .any(|r| matches!(r, FormRow::Check(l, true) if *l == label))
}

#[cfg(test)]
mod tests {
    use super::super::find_files::Found;

    use super::super::{App, Filter, Mode, Step};

    use crate::{KeyCode, KeyEvent, KeyModifiers};

    use std::sync::Arc;
    use std::sync::mpsc;

    use tc_core::find;

    use tc_core::panel::Panel;

    use super::super::tests::{key, rows, two_panels, type_keys, type_text, wait, with};

    #[test]
    fn edit_save_exit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi  \r\n").unwrap();
        let p = Panel::open(dir.path()).unwrap();
        let mut app = App::new(p, Panel::open(dir.path()).unwrap());
        key(&mut app, KeyCode::Down); // skip ".."
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Edit { .. }));
        key(&mut app, KeyCode::Char('x'));
        key(&mut app, KeyCode::F(2));
        key(&mut app, KeyCode::Char('n')); // keep CRLF
        // small local file: saved inline, no worker frame that would show the panels
        assert!(matches!(app.mode, Mode::Edit { error: None, .. }));
        key(&mut app, KeyCode::Char('x'));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::AskExit(_)));
        key(&mut app, KeyCode::Enter); // Enter = yes
        assert!(matches!(
            app.mode,
            Mode::AskSave {
                step: Step::Eol,
                ..
            }
        ));
        key(&mut app, KeyCode::Enter); // to LF
        wait(&mut app);
        assert!(matches!(app.mode, Mode::Normal));
        let text = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(text, "xxhi\n");
    }

    #[test]
    fn large_file_asks_for_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::fs::File::create(dir.path().join("big")).unwrap();
        f.set_len((1 << 30) + 1).unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        assert!(matches!(app.mode, Mode::AskViewer { .. }));
        key(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.mode, Mode::View { .. }));
        app.mode = Mode::Normal;
        key(&mut app, KeyCode::F(4));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }

    /// Write `text` to `f` with a later modification time, as another program would.
    fn write_later(f: &std::path::Path, text: &str, secs: u64) {
        std::fs::write(f, text).unwrap();
        let t = std::time::SystemTime::now() + std::time::Duration::from_secs(secs);
        let file = std::fs::File::options().write(true).open(f).unwrap();
        file.set_modified(t).unwrap();
    }

    /// Run the watch now instead of waiting a second.
    fn watch(app: &mut App) {
        app.watch_at = std::time::Instant::now() - std::time::Duration::from_secs(2);
        app.tick();
    }

    #[test]
    fn panels_follow_changes_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b"), "").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down); // cursor on "b"
        watch(&mut app); // records the directory time
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("a"), "").unwrap();
        watch(&mut app);
        while app.panels[0].entries().len() < 3 {
            app.tick();
        }
        assert_eq!(app.panels[0].current().unwrap().name, "b"); // cursor stays on its entry
    }

    #[test]
    fn editor_and_viewer_follow_changes_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "one\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        // no edits: read again silently
        write_later(&f, "two\n", 5);
        watch(&mut app);
        assert_eq!(rows(&mut app), ["two", ""]);
        // unsaved edits: asks once; n keeps mine
        key(&mut app, KeyCode::Char('x'));
        write_later(&f, "three\n", 10);
        watch(&mut app);
        assert!(matches!(
            app.mode,
            Mode::AskSave {
                step: Step::Changed,
                ..
            }
        ));
        key(&mut app, KeyCode::Char('n'));
        watch(&mut app);
        assert!(matches!(app.mode, Mode::Edit { .. }));
        assert_eq!(rows(&mut app), ["xtwo", ""]);
        // F2 asks again; r reloads from disk
        key(&mut app, KeyCode::F(2));
        key(&mut app, KeyCode::Char('r'));
        assert_eq!(rows(&mut app), ["three", ""]);
        key(&mut app, KeyCode::Esc);
        // viewer: appended lines show up
        key(&mut app, KeyCode::F(3));
        write_later(&f, "three\nfour\n", 15);
        watch(&mut app);
        let Mode::View { view, .. } = &mut app.mode else {
            panic!("not in viewer")
        };
        assert_eq!(view.lines(5, 30).unwrap(), ["three", "four"]);
    }

    #[test]
    fn editor_shift_up_down_selects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        with(&mut app, KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(
            app.mode.editor_mut().unwrap().copy().as_deref(),
            Some("one\ntwo\n")
        );
        with(&mut app, KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(
            app.mode.editor_mut().unwrap().copy().as_deref(),
            Some("one\n")
        );
        key(&mut app, KeyCode::Down);
        assert_eq!(app.mode.editor_mut().unwrap().copy(), None);
        // Option+arrow (Alt) moves and deletes by word like Ctrl (macOS keeps Ctrl+arrow)
        with(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (3, 0));
        with(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (2, 0));
        with(&mut app, KeyCode::Delete, KeyModifiers::ALT);
        assert_eq!(rows(&mut app), ["one", "two", ""]);
    }

    #[test]
    fn editor_replace_each_and_reload_warning() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "ab ab\nab\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
        type_keys(&mut app, "ab");
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "X");
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Char('y')); // first ab -> X
        key(&mut app, KeyCode::Char('n')); // second skipped
        key(&mut app, KeyCode::Char('a')); // rest
        assert!(app.replacing.is_none());
        let ed = app.mode.editor_mut().unwrap();
        assert_eq!(ed.visible(5, 9), ["X ab", "X", ""]);

        // someone else writes the file: saving asks first
        std::fs::write(&f, "other\n").unwrap();
        let t = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t)
            .unwrap();
        key(&mut app, KeyCode::F(2));
        assert!(matches!(
            app.mode,
            Mode::AskSave {
                step: Step::Changed,
                ..
            }
        ));
        key(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::Edit { .. }));
        key(&mut app, KeyCode::F(2));
        key(&mut app, KeyCode::Char('y'));
        wait(&mut app);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "X ab\nX\n");
    }

    /// F2 on any file (not only Markdown) trims trailing blanks and ends with a line break,
    /// on disk and on screen.
    #[test]
    fn editor_save_trims_any_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x  \ny \t\nz ").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        key(&mut app, KeyCode::F(2));
        wait(&mut app);
        app.tick();
        assert_eq!(rows(&mut app), ["x", "y", "z", ""]);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "x\ny\nz\n");
    }

    /// VS Code keys: Ctrl+X without a selection cuts the line, Ctrl+V puts it above the cursor
    /// line, Alt+Up/Down move lines, Alt+Shift+Down copies the line, Ctrl+Enter opens a line.
    #[test]
    fn editor_vscode_line_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(rows(&mut app), ["two", "three", ""]);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Right);
        with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(rows(&mut app), ["two", "one", "three", ""]);
        app.on_paste("one\r\n".into()); // the host paste event of the same line
        assert_eq!(rows(&mut app), ["two", "one", "one", "three", ""]);
        with(&mut app, KeyCode::Up, KeyModifiers::ALT);
        assert_eq!(rows(&mut app), ["two", "one", "three", "one", ""]);
        with(
            &mut app,
            KeyCode::Down,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        let all = |app: &mut App| app.mode.editor_mut().unwrap().visible(20, 80);
        assert_eq!(all(&mut app), ["two", "one", "three", "three", "one", ""]);
        with(&mut app, KeyCode::Enter, KeyModifiers::CONTROL);
        type_keys(&mut app, "x");
        assert_eq!(
            all(&mut app),
            ["two", "one", "three", "three", "x", "one", ""]
        );
    }

    #[test]
    fn editor_find_replace_undo_clipboard() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo one\n").unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);

        key(&mut app, KeyCode::F(7));
        type_keys(&mut app, "(");
        key(&mut app, KeyCode::Enter);
        let err = app.search_form.as_ref().unwrap().error.as_deref();
        assert!(err.is_some_and(|e| !e.is_empty()));
        key(&mut app, KeyCode::Backspace);
        assert!(app.search_form.as_ref().unwrap().error.is_none());
        type_keys(&mut app, "ONE"); // smart case: uppercase is exact, no match
        key(&mut app, KeyCode::Enter);
        assert!(app.search_form.is_none());
        assert_eq!(app.note, "not found");
        key(&mut app, KeyCode::F(7));
        key(&mut app, KeyCode::Esc);
        assert!(app.search_form.is_none());
        key(&mut app, KeyCode::F(7));
        type_keys(&mut app, "one");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (0, 0));
        with(&mut app, KeyCode::F(7), KeyModifiers::SHIFT); // next match
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 4));
        with(&mut app, KeyCode::F(7), KeyModifiers::SHIFT);
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (0, 0));

        let ctrl =
            |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        ctrl(&mut app, 'c'); // match is selected
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::End);
        ctrl(&mut app, 'v');
        assert_eq!(rows(&mut app), ["one", "two oneone", ""]);
        ctrl(&mut app, 'z');
        assert_eq!(rows(&mut app), ["one", "two one", ""]);

        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        type_keys(&mut app, "o(n)e");
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "[${1}]");
        assert!(app.search_form.is_some());
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.note, "2 replaced");
        assert_eq!(rows(&mut app), ["[n]", "two [n]", ""]);

        ctrl(&mut app, 'g');
        type_text(&mut app, "2");
        assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 0));
    }

    #[test]
    fn editor_search_regex_groups_and_literal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "ab a+b\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        // regex on (default): groups are referenced in Replace with
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        type_keys(&mut app, "(a)(b)");
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "$2$1");
        key(&mut app, KeyCode::Enter);
        assert_eq!(rows(&mut app)[0], "ba a+b");
        // regex off: Find and Replace with are plain text, `$0` included
        with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        type_keys(&mut app, "a+b");
        key(&mut app, KeyCode::Tab);
        type_keys(&mut app, "[$0]");
        for _ in 0..3 {
            key(&mut app, KeyCode::Tab); // Replace, Confirm each, Regex
        }
        key(&mut app, KeyCode::Char(' '));
        key(&mut app, KeyCode::Enter);
        assert_eq!(rows(&mut app)[0], "ba [$0]");
    }

    #[test]
    fn search_dialog_f1_shows_regex_help() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        key(&mut app, KeyCode::F(7));
        assert!(app.search_form.is_some());
        key(&mut app, KeyCode::F(1));
        assert!(app.help);
        assert_eq!(app.help_text().0, "Help: search");
        assert!(app.help_entries().iter().any(|(l, _)| l.contains("group")));
    }

    #[test]
    fn viewer_next_prev_file() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["a.txt", "b.png", "c.txt"] {
            std::fs::write(dir.path().join(n), "x").unwrap();
        }
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        let a = app.panels[0]
            .entries()
            .iter()
            .position(|e| e.name == "a.txt")
            .unwrap();
        app.panels[0].set_cursor(a);
        key(&mut app, KeyCode::F(3));
        key(&mut app, KeyCode::Char('h'));
        let alt = |app: &mut App, code| with(app, code, KeyModifiers::ALT);
        let title = |app: &App| match &app.mode {
            Mode::View { title, view } => (title.clone(), view.hex()),
            _ => panic!("not in viewer"),
        };
        alt(&mut app, KeyCode::Right);
        assert!(title(&app).0.ends_with("c.txt") && title(&app).1);
        assert_eq!(app.panels[0].current().unwrap().name, "c.txt");
        alt(&mut app, KeyCode::Right);
        assert_eq!(app.note, "no next file");
        assert!(title(&app).0.ends_with("c.txt"));
        alt(&mut app, KeyCode::Left);
        assert!(title(&app).0.ends_with("a.txt"));
        alt(&mut app, KeyCode::Left);
        assert_eq!(app.note, "no previous file");

        // Ctrl+H is not the hex toggle
        with(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert!(title(&app).1);

        app.open_viewer(dir.path().join("c.txt"));
        app.panels[0].set_cursor(a);
        alt(&mut app, KeyCode::Right);
        assert_eq!(app.note, "next / previous file: panel files only");

        // viewer opened from a dialog: panel cursor may match, still no stepping
        app.note.clear();
        app.panels[0].set_cursor(a);
        app.open_viewer(dir.path().join("a.txt"));
        app.found = Some(Found {
            list: Vec::new(),
            view: Vec::new(),
            filter: Filter::default(),
            sort: None,
            sel: 0,
            running: false,
            remote: false,
            error: Arc::default(),
            re: None,
            query: find::Query::default(),
            rx: mpsc::channel().1,
            cancel: Arc::default(),
        });
        alt(&mut app, KeyCode::Right);
        assert_eq!(app.note, "next / previous file: panel files only");
    }

    #[test]
    fn viewer_hex_goto_offset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.bin"), [b'a'; 64]).unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        key(&mut app, KeyCode::Char('h'));
        key(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "0x20");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode_view_percent(), 50);
    }

    #[test]
    fn viewer_search_and_goto() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nfoo\nc\nfoo\n").unwrap();
        let mut app = App::new(
            Panel::open(dir.path()).unwrap(),
            Panel::open(dir.path()).unwrap(),
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        key(&mut app, KeyCode::Char('f')); // same as '/'
        type_text(&mut app, "foo");
        let top = |app: &mut App| match &mut app.mode {
            Mode::View { view, .. } => view.lines(1, 10).unwrap(),
            _ => panic!("not in viewer"),
        };
        assert_eq!(top(&mut app), ["foo"]);
        assert_eq!(app.mode_view_percent(), 28);
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(app.mode_view_percent(), 71);
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(app.note, "not found");
        key(&mut app, KeyCode::Char('N'));
        assert_eq!(app.mode_view_percent(), 28);
        key(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "1");
        assert_eq!(app.mode_view_percent(), 0);
        key(&mut app, KeyCode::Char('g'));
        type_text(&mut app, "x%");
        assert!(app.input.as_ref().unwrap().error.is_some());
    }

    #[test]
    fn viewer_f4_edits_at_top_line_and_shared_keys_work() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\nb\nFoo\nc\nfoo\n").unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::F(3));
        with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        type_text(&mut app, "foo");
        assert_eq!(app.mode_view_percent(), 28);
        with(&mut app, KeyCode::F(7), KeyModifiers::SHIFT);
        assert_eq!(app.mode_view_percent(), 71);
        with(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(app.note, "case sensitive");
        with(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        type_text(&mut app, "2");
        key(&mut app, KeyCode::F(4));
        wait(&mut app);
        assert_eq!(app.mode.editor_mut().unwrap().cursor.0, 1);
        // Ctrl+S saves like F2
        key(&mut app, KeyCode::Char('x'));
        with(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        wait(&mut app);
        assert!(std::fs::read_to_string(&file).unwrap().starts_with("a\nxb"));
    }
}

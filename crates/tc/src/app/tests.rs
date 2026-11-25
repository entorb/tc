use super::menu::{
    EDIT_FOOTER, FIND_FORM_FOOTER, FORM_FOOTER, FOUND_FOOTER, Footer, RENAME_FOOTER,
    SETTINGS_FOOTER, SHIFT_FOOTER, SYNC_FOOTER, VIEW_FOOTER,
};
use super::*;
use crate::KeyModifiers;
use crate::fmt;

pub(super) fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

pub(super) fn two_panels(l: &Path, r: &Path) -> App {
    App::new(Panel::open(l).unwrap(), Panel::open(r).unwrap())
}

pub(super) fn with(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
}

#[test]
fn set_caret_moves_cursor_by_chars_and_clamps_without_editing() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Char(':'));
    for c in "aé日b".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let cur = |app: &App| match &app.mode {
        Mode::Cmd(l) => l.cur,
        _ => usize::MAX,
    };
    app.set_caret(2);
    assert_eq!(cur(&app), 3);
    app.set_caret(99);
    assert_eq!(cur(&app), "aé日b".len());
    app.set_caret(0);
    assert_eq!(cur(&app), 0);
}

#[test]
fn left_right_navigate_dirs_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("d")).unwrap();
    std::fs::write(dir.path().join("f"), "").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    let name = |app: &App| app.panels[0].current().map(|e| e.name.clone());
    key(&mut app, KeyCode::Down); // "d"
    assert_eq!(name(&app), Some("d".into()));
    key(&mut app, KeyCode::Right);
    assert_eq!(app.panels[0].path(), dir.path().join("d"));
    key(&mut app, KeyCode::Left);
    assert_eq!(
        (app.panels[0].path(), name(&app)),
        (dir.path(), Some("d".into()))
    );
    key(&mut app, KeyCode::Down); // "f"
    key(&mut app, KeyCode::Right);
    assert_eq!(app.panels[0].path(), dir.path());
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn zoom_steps_and_clamps() {
    assert!((zoom_step(1.0, 1) - 1.1).abs() < 1e-6);
    assert!((zoom_step(0.5, -1) - 0.5).abs() < 1e-6);
    assert!((zoom_step(3.0, 1) - 3.0).abs() < 1e-6);
}

#[test]
fn prompt_edits_at_cursor_and_rename_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::F(7));
    for c in "ac".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Char('b'));
    key(&mut app, KeyCode::Enter);
    assert!(dir.path().join("abc").is_dir());
    key(&mut app, KeyCode::Down);
    with(&mut app, KeyCode::F(6), KeyModifiers::SHIFT);
    with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_text(&mut app, "xyz");
    wait(&mut app);
    assert!(dir.path().join("xyz").is_dir() && !dir.path().join("abc").exists());
}

#[test]
fn space_on_dir_calculates_size() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("d/e")).unwrap();
    std::fs::write(dir.path().join("d/a"), "123").unwrap();
    std::fs::write(dir.path().join("d/e/b"), "45").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down); // "d"
    key(&mut app, KeyCode::Char(' '));
    while app.dir_job.is_some() {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(app.panels[0].dir_size(&app.panels[0].entries()[1]), Some(5));
    assert_eq!(app.panels[0].selection_info(), (1, 5));
    // the right panel shows the same directory but was not asked
    assert_eq!(app.panels[1].dir_size(&app.panels[1].entries()[1]), None);
}

#[test]
fn f10_quits_letters_start_quick_search() {
    let tmp = tempfile::tempdir().unwrap();
    for n in ["alpha", "quux"] {
        std::fs::write(tmp.path().join(n), "").unwrap();
    }
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::Char('q'));
    assert!(matches!(&app.mode, Mode::Search(t) if t == "q") && !app.quit);
    assert_eq!(app.panels[0].current().unwrap().name, "quux");
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('*')); // keys with a meaning keep it
    assert!(matches!(app.mode, Mode::Normal));
    key(&mut app, KeyCode::F(10));
    assert!(app.quit);
}

#[test]
fn footer_keys_are_documented_in_their_help() {
    let screens: [(&Footer, &[&str]); 11] = [
        (&RENAME_FOOTER, &[fmt::HELP_RENAME]),
        (&FOOTER, &[fmt::HELP_MAIN]),
        (&VIEW_FOOTER, &[fmt::HELP_VIEW]),
        (&EDIT_FOOTER, &[fmt::HELP_EDIT]),
        (&FOUND_FOOTER, &[fmt::HELP_FOUND]),
        (&SYNC_FOOTER, &[fmt::HELP_SYNC]),
        (&FIND_FORM_FOOTER, &[fmt::HELP_FIND_FORM]),
        (
            &FORM_FOOTER,
            &[fmt::HELP_RENAME, fmt::HELP_SYNC_FORM, fmt::HELP_PROPS],
        ),
        (&SETTINGS_FOOTER, &[fmt::HELP_SETTINGS]),
        (&STATS_FOOTER, &[fmt::HELP_STATS]),
        (&PROCS_FOOTER, &[fmt::HELP_PROCS]),
    ];
    for (footer, helps) in screens {
        // every screen: F1 help first, F10 close last, F-keys only up to F12
        assert_eq!(footer.first().map(|f| f.1), Some(KeyCode::F(1)));
        assert_eq!(footer.last().map(|f| f.1), Some(KeyCode::F(10)));
        assert!(footer.iter().all(|f| !matches!(f.1, KeyCode::F(13..))));
        let shift = [&SHIFT_FOOTER[..]].into_iter().filter(|_| {
            std::ptr::eq(footer, &FOOTER[..]) || std::ptr::eq(footer, &STATS_FOOTER[..])
        });
        for &(label, ..) in footer.iter().chain(shift.flatten()) {
            // F1 opens the help itself
            if label.starts_with("F1 ") {
                continue;
            }
            for help in helps {
                assert!(
                    !fmt::key_help(help, label).is_empty(),
                    "{label} missing in help"
                );
            }
        }
    }
}

#[test]
fn command_line_runs_only_on_enter() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    for c in ":ls".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert!(app.run.is_none() && !app.quit);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.run, Some(("ls".into(), tmp.path().to_owned(), None)));
    assert_eq!(app.history, ["ls"]);
    key(&mut app, KeyCode::Char(':'));
    key(&mut app, KeyCode::Up);
    assert!(matches!(&app.mode, Mode::Cmd(l) if l.text == "ls"));
    key(&mut app, KeyCode::Esc);
    app.run = None;
    key(&mut app, KeyCode::Char(':'));
    key(&mut app, KeyCode::Enter);
    assert!(app.run.is_none());
}

pub(super) fn settle(app: &mut App) {
    wait(app);
    while app.panels.iter().any(|p| p.loading().is_some()) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

pub(super) fn type_keys(app: &mut App, text: &str) {
    text.chars().for_each(|c| key(app, KeyCode::Char(c)));
}

#[test]
fn archive_enter_view_copy_out_and_read_only() {
    use std::io::Write;
    let (left, right) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = std::fs::File::create(left.path().join("a.zip")).unwrap();
    let mut z = tc_core_zip(file);
    z.0.start_file("d/f.txt", z.1).unwrap();
    z.0.write_all(b"hello").unwrap();
    z.0.finish().unwrap();
    let mut app = App::new(
        Panel::open(left.path()).unwrap(),
        Panel::open(right.path()).unwrap(),
    );
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter); // opens a.zip
    wait(&mut app);
    assert!(app.panels[0].archive().is_some());
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter); // d
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::F(3));
    wait(&mut app);
    assert!(matches!(app.mode, Mode::View { .. }) && app.view_tmp.is_some());
    key(&mut app, KeyCode::Esc);
    app.tick();
    assert!(app.view_tmp.is_none());
    key(&mut app, KeyCode::F(6));
    assert!(matches!(app.mode, Mode::Message(_)));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert_eq!(std::fs::read(right.path().join("f.txt")).unwrap(), b"hello");
}

fn tc_core_zip(f: std::fs::File) -> (zip::ZipWriter<std::fs::File>, zip::write::SimpleFileOptions) {
    (
        zip::ZipWriter::new(f),
        zip::write::SimpleFileOptions::default(),
    )
}

pub(super) fn wait(app: &mut App) {
    while matches!(app.mode, Mode::Work(_))
        || matches!(&app.mode, Mode::Xfer(x) if x.ask.is_none() && x.error.is_none())
    {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

pub(super) fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
    key(app, KeyCode::Enter);
}

pub(super) fn rows(app: &mut App) -> Vec<String> {
    app.mode.editor_mut().unwrap().visible(5, 30)
}

#[test]
fn sync_browsing_follows_enter_and_parent_and_stops_when_missing() {
    let (l, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for d in [l.path(), r.path()] {
        std::fs::create_dir_all(d.join("a/sub")).unwrap();
    }
    std::fs::create_dir(l.path().join("only")).unwrap();
    let mut app = two_panels(l.path(), r.path());
    with(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    assert!(app.sync_browse);
    key(&mut app, KeyCode::Down); // "a" (after "..")
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panels[0].path(), l.path().join("a"));
    assert_eq!(app.panels[1].path(), r.path().join("a"));
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.panels[0].path(), l.path());
    assert_eq!(app.panels[1].path(), r.path());
    // "only" is missing on the right: left goes, sync turns off
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.panels[0].current().unwrap().name, "only");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panels[0].path(), l.path().join("only"));
    assert_eq!(app.panels[1].path(), r.path());
    assert!(!app.sync_browse && matches!(app.mode, Mode::Message(_)));
}

#[test]
fn dir_history_lists_newest_first_and_steps_there() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    app.go(0, a.clone());
    settle(&mut app);
    app.go(0, b.clone());
    settle(&mut app);
    with(&mut app, KeyCode::Down, KeyModifiers::ALT);
    let Mode::DirHistory(rows, 0) = &app.mode else {
        panic!("no history list")
    };
    assert_eq!(rows[..2], [b.clone(), a.clone()]);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert_eq!(app.panels[0].path(), a);
    // a history step: forward still leads to b
    with(&mut app, KeyCode::Right, KeyModifiers::ALT);
    settle(&mut app);
    assert_eq!(app.panels[0].path(), b);
}

#[test]
fn delete_is_f8_and_ctrl_arrow_opens_cursor_dir_in_other_panel() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("d")).unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    settle(&mut app);
    app.active = 1;
    let row = app.panels[1]
        .entries()
        .iter()
        .position(|e| e.name == "d")
        .unwrap();
    app.panels[1].set_cursor(row);
    // Ctrl+Right from the right panel points away from the left one: nothing
    with(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
    settle(&mut app);
    assert_eq!(app.panels[0].path(), tmp.path());
    with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
    settle(&mut app);
    assert_eq!(app.panels[0].path(), tmp.path().join("d"));
    assert_eq!(app.active, 1);
    key(&mut app, KeyCode::Delete);
    assert!(matches!(app.mode, Mode::Confirm(Op::Delete, ..)));
}

#[test]
fn shift_up_down_marks_and_unmarks() {
    let tmp = tempfile::tempdir().unwrap();
    for f in ["a", "b", "c"] {
        std::fs::write(tmp.path().join(f), "").unwrap();
    }
    let mut app = two_panels(tmp.path(), tmp.path());
    key(&mut app, KeyCode::Down); // skip ".."
    let shift = |app: &mut App, code| with(app, code, KeyModifiers::SHIFT);
    shift(&mut app, KeyCode::Down);
    shift(&mut app, KeyCode::Down);
    assert_eq!(app.panels[0].selection_info().0, 2); // a, b
    shift(&mut app, KeyCode::Up); // c toggled on, back on b
    shift(&mut app, KeyCode::Up); // b off
    shift(&mut app, KeyCode::Up); // a off
    assert_eq!(app.panels[0].selection_info().0, 1); // only c
    assert_eq!(app.panels[0].current().unwrap().name, "..");
    // Shift+Home/End mark the range; again on a fully marked range unmark it
    key(&mut app, KeyCode::End); // on c (marked)
    shift(&mut app, KeyCode::Home); // a, b get marked too
    assert_eq!(app.panels[0].selection_info().0, 3);
    shift(&mut app, KeyCode::End); // from "..": all marked already
    assert_eq!(app.panels[0].selection_info().0, 0);
    shift(&mut app, KeyCode::Home); // from c: a, b, c again
    assert_eq!(app.panels[0].selection_info().0, 3);
    key(&mut app, KeyCode::End);
    // Shift+PgUp toggles each entry passed and stops at the top
    app.page = 10;
    shift(&mut app, KeyCode::PageUp); // c, b, a off, ".." stays unmarked
    assert_eq!(app.panels[0].selection_info().0, 0);
    assert_eq!(app.panels[0].cursor(), 0);
}

#[test]
fn quick_search_up_down_jump_between_matches() {
    let tmp = tempfile::tempdir().unwrap();
    for n in ["apple", "apricot", "banana", "a_b"] {
        std::fs::write(tmp.path().join(n), "").unwrap();
    }
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::Char('a'));
    assert_eq!(app.panels[0].current().unwrap().name, "a_b");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.panels[0].current().unwrap().name, "apricot");
    assert!(matches!(&app.mode, Mode::Search(t) if t == "a"));
    key(&mut app, KeyCode::Up);
    assert_eq!(app.panels[0].current().unwrap().name, "apple");
    // `_` extends the search instead of ending it
    key(&mut app, KeyCode::Char('_'));
    assert_eq!(app.panels[0].current().unwrap().name, "a_b");
    assert!(matches!(&app.mode, Mode::Search(t) if t == "a_"));
}

use super::find_files::{
    F_CONTENT, F_CONTENT_ON, F_EVERY, F_FOLLOW, F_HIDDEN, F_MIN, F_NAME, F_SAVED,
};
use super::menu::{
    EDIT_FOOTER, FIND_FORM_FOOTER, FORM_FOOTER, FOUND_FOOTER, Footer, N, SHIFT_FOOTER, SYNC_FOOTER,
    VIEW_FOOTER,
};
use super::multi_rename::{R_NAME, R_PRESET, RENAME_NAMES_FILE};
use super::*;
use crate::fmt;
use ratatui::crossterm::event::KeyModifiers;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use tc_core::find;
use tc_core::find::Hit;
use tc_core::panel::Entry;
use tc_core::procs::ProcSort;
use tc_core::vfs::Side;

fn key(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn two_panels(l: &Path, r: &Path) -> App {
    App::new(Panel::open(l).unwrap(), Panel::open(r).unwrap())
}

fn with(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    app.on_key(KeyEvent::new(code, mods));
}

#[test]
fn tabs_open_cycle_close_and_swap() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    let ctrl = KeyModifiers::CONTROL;
    with(&mut app, KeyCode::Char('t'), ctrl);
    assert_eq!((app.tabs[0].len(), app.tab[0]), (1, 1));
    app.go(0, sub.clone());
    with(&mut app, KeyCode::PageDown, ctrl);
    assert_eq!((app.panels[0].path(), app.tab[0]), (dir.path(), 0));
    with(&mut app, KeyCode::PageUp, ctrl);
    assert_eq!((app.panels[0].path(), app.tab[0]), (sub.as_path(), 1));
    with(&mut app, KeyCode::Char('u'), ctrl);
    assert_eq!(
        (app.tabs[1].len(), app.tab[1], app.tabs[0].len()),
        (1, 1, 0)
    );
    key(&mut app, KeyCode::Tab);
    with(&mut app, KeyCode::Char('w'), ctrl);
    assert_eq!((app.panels[1].path(), app.tab[1]), (dir.path(), 0));
    assert!(app.tabs[1].is_empty());
    with(&mut app, KeyCode::Char('w'), ctrl); // last tab stays
    assert_eq!(app.panels[1].path(), dir.path());
    app.go(0, sub.clone());
    app.active = 0;
    with(&mut app, KeyCode::Char('t'), ctrl);
    app.go(0, dir.path().to_owned());
    app.close_tab(0, 0); // the first tab, while the second is shown
    assert_eq!((app.panels[0].path(), app.tabs[0].len()), (dir.path(), 0));
    app.close_tab(0, 0); // last one stays
    assert_eq!(app.panels[0].path(), dir.path());
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
fn f2_renames_one_or_multi_renames_several() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["a", "b"] {
        std::fs::write(dir.path().join(n), "").unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::F(2));
    assert!(matches!(app.mode, Mode::Transfer(Op::Move, _, _)));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::F(2));
    assert!(matches!(app.mode, Mode::MultiRename(_)));
}

#[test]
fn delete_confirm_text_trash_choice_and_permanent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("d")).unwrap();
    for n in ["a", "b", "c"] {
        std::fs::write(dir.path().join(n), "").unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    for c in "+".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    type_text(&mut app, ""); // select all files
    key(&mut app, KeyCode::Down); // "d"
    key(&mut app, KeyCode::Char(' ')); // select the dir too
    key(&mut app, KeyCode::F(8));
    let Mode::Confirm(_, _, text) = &app.mode else {
        panic!("no confirm")
    };
    assert!(text.contains("recursively") && text.contains('…'));
    key(&mut app, KeyCode::Esc);
    assert!(dir.path().join("a").exists() && dir.path().join("d").exists());
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('p'));
    assert!(!dir.path().join("a").exists() && !dir.path().join("d").exists());
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
fn multi_rename_counter_history_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(f), f).unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down); // skip ".."
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert);
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    for _ in 0..3 {
        key(&mut app, KeyCode::Backspace); // "[N]"
    }
    for c in "x_[Q]".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert!(f.error.as_deref().is_some_and(|e| e.contains("[Q]")));
    key(&mut app, KeyCode::Enter); // refused, form stays
    assert!(matches!(app.mode, Mode::MultiRename(_)));
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Backspace);
    key(&mut app, KeyCode::Char('C'));
    key(&mut app, KeyCode::Char(']'));
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!(f.preview[1].1.as_deref(), Ok("x_2.txt"));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    let read = |n: &str| std::fs::read_to_string(dir.path().join(n)).unwrap();
    assert_eq!(
        (read("x_1.txt"), read("x_2.txt")),
        ("a.txt".into(), "b.txt".into())
    );
    assert!(dir.path().join("c.txt").exists() && !dir.path().join("a.txt").exists());
    // reopens with the last settings; Alt+Down past the history gives the defaults
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!(form_text(&f.rows, R_NAME), "x_[C]");
    with(&mut app, KeyCode::Down, KeyModifiers::ALT);
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!(form_text(&f.rows, R_NAME), "[N]");
    assert_eq!(f.preview[0].1.as_deref(), Ok(f.preview[0].0.as_str()));
    key(&mut app, KeyCode::Enter); // nothing changes: refused
    assert!(matches!(&app.mode, Mode::MultiRename(f) if f.error.is_some()));
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn swap_root_and_sort_click() {
    let (l, r) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(l.path().join(".h"), "").unwrap();
    let mut app = two_panels(l.path(), r.path());
    app.areas = [Rect::new(0, 0, 40, 10), Rect::new(40, 0, 40, 10)];
    with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    assert_eq!(app.panels[0].path(), r.path());
    with(&mut app, KeyCode::Char('='), KeyModifiers::ALT); // right gets left's dir
    assert_eq!(app.panels[1].path(), r.path());
    with(&mut app, KeyCode::Char('\\'), KeyModifiers::CONTROL);
    settle(&mut app); // listing may take over the 50 ms wait on a loaded machine
    assert!(app.panels[0].path().parent().is_none());
    app.on_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 40 + 20,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.panels[1].sort().0, SortKey::Size);
    with(&mut app, KeyCode::F(5), KeyModifiers::CONTROL); // active panel is 1 now: by date
    assert_eq!(app.panels[1].sort().0, SortKey::Date);
}

#[test]
fn clipboard_and_paste() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down);
    with(&mut app, KeyCode::Insert, KeyModifiers::CONTROL);
    assert_eq!(
        app.copied.take(),
        Some(dir.path().join("a.txt").display().to_string())
    );
    key(&mut app, KeyCode::F(4));
    wait(&mut app);
    with(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.copied.as_deref(), Some("o"));
    app.on_paste("a\r\nb".into());
    assert_eq!(rows(&mut app)[..2], ["a", "bne"]);
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
fn stats_deletes_without_rescan_and_runs_cmd_in_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("many")).unwrap();
    std::fs::write(tmp.path().join("many").join("a"), "x").unwrap();
    std::fs::write(tmp.path().join("big"), "x".repeat(100)).unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('n'));
    assert!(tmp.path().join("big").exists());
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('p'));
    app.tick();
    assert!(!tmp.path().join("big").exists());
    match &app.mode {
        Mode::Stats(st) => {
            assert_eq!((st.root.size, st.root.files), (1, 1));
            assert_eq!(st.current().children[0].name, "many");
        }
        _ => panic!("not in stats"),
    }
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "ls");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.run, Some(("ls".into(), tmp.path().join("many"), None)));
    app.tick();
    assert!(matches!(app.mode, Mode::Stats(_)));
}

fn stats_app(dir: &Path) -> App {
    let mut app = two_panels(dir, dir);
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    app
}

#[test]
fn stats_feeds_entries_to_panel() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a"), "x".repeat(30)).unwrap();
    std::fs::write(tmp.path().join("b"), "x".repeat(20)).unwrap();
    std::fs::create_dir(tmp.path().join("c")).unwrap();
    let names = |app: &App| -> Vec<PathBuf> {
        let p = &app.panels[app.active];
        assert!(p.is_virtual());
        p.entries().iter().map(|e| PathBuf::from(&e.name)).collect()
    };
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Insert); // b
    key(&mut app, KeyCode::Char('p'));
    assert!(matches!(app.mode, Mode::Normal));
    settle(&mut app);
    assert_eq!(names(&app), [tmp.path().join("b")]);
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Char('p'));
    settle(&mut app);
    let mut all = names(&app);
    all.sort();
    assert_eq!(all, ["a", "b", "c"].map(|n| tmp.path().join(n)));
}

#[test]
fn stats_marks_and_deletes_all_marked() {
    let tmp = tempfile::tempdir().unwrap();
    for (n, size) in [("a", 30), ("b", 20), ("c", 10)] {
        std::fs::write(tmp.path().join(n), "x".repeat(size)).unwrap();
    }
    #[cfg(target_os = "linux")] // macOS APFS rejects non-UTF-8 names (EILSEQ)
    let odd = {
        use std::os::unix::ffi::OsStrExt;
        let odd = OsStr::from_bytes(b"f\xff");
        std::fs::write(tmp.path().join(odd), "x").unwrap();
        odd
    };
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Char(' ')); // a, moves down
    key(&mut app, KeyCode::Insert); // b
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Insert); // b unmarked again, moves down
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Insert); // b marked
    key(&mut app, KeyCode::F(8));
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert!(st.confirm.as_ref().is_some_and(|t| t.contains("2 items (")));
    key(&mut app, KeyCode::Char('p'));
    app.tick();
    assert!(!tmp.path().join("a").exists() && !tmp.path().join("b").exists());
    assert!(tmp.path().join("c").exists());
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert_eq!(
        st.root.size,
        if cfg!(target_os = "linux") { 11 } else { 10 }
    );
    assert!(st.marked.is_empty());
    #[cfg(target_os = "linux")]
    {
        assert!(tmp.path().join(odd).exists());
        assert_eq!(st.root.files, 2);
    }
}

#[cfg(unix)]
#[test]
fn stats_batch_stops_at_first_error() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a"), "x".repeat(30)).unwrap();
    let ro = tmp.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::write(ro.join("f"), "x".repeat(20)).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    let restore = || std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755));
    if std::fs::write(ro.join("probe"), "").is_ok() {
        return restore().unwrap(); // root ignores permissions
    }
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Insert); // a
    key(&mut app, KeyCode::Insert); // ro
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('p'));
    assert!(matches!(app.mode, Mode::Message(_)));
    app.tick();
    key(&mut app, KeyCode::Esc);
    app.tick();
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert!(!tmp.path().join("a").exists() && ro.join("f").exists());
    let names: Vec<_> = st.root.children.iter().map(|n| n.name.clone()).collect();
    assert_eq!(names, ["ro"]);
    assert!(st.marked.contains(OsStr::new("ro")));
    restore().unwrap();
}

#[test]
fn disconnect_all_closes_every_remote_hold() {
    let left = tempfile::tempdir().unwrap();
    let mem = tc_core::vfs::mem::Mem::new();
    let mut app = two_panels(left.path(), left.path());
    app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
    settle(&mut app);
    app.active = 1;
    with(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL); // tab on the same session
    settle(&mut app);
    assert!(Arc::strong_count(&mem) > 2);
    with(&mut app, KeyCode::Null, KeyModifiers::SHIFT);
    settle(&mut app);
    assert_eq!(Arc::strong_count(&mem), 1);
    assert_eq!(app.panels[1].path(), left.path());
    assert!(app.panels[1].remote().is_none());
}

#[test]
fn remote_stats_deletes_all_marked() {
    let left = tempfile::tempdir().unwrap();
    let mem = tc_core::vfs::mem::Mem::new();
    for (n, size) in [("/a", 3), ("/b", 2), ("/c", 1)] {
        mem.put(n, &b"x".repeat(size));
    }
    let mut app = two_panels(left.path(), left.path());
    app.active = 1;
    app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
    settle(&mut app);
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('p'));
    settle(&mut app);
    app.tick();
    assert!(mem.get("/a").is_none() && mem.get("/b").is_none() && mem.get("/c").is_some());
    assert!(
        matches!(&app.mode, Mode::Stats(st) if st.root.size == 1 && st.current().children.len() == 1)
    );
}

#[test]
fn slash_filters_stats_and_acts_on_shown_rows_only() {
    let tmp = tempfile::tempdir().unwrap();
    for (n, size) in [("Alpha", 30), ("beta", 20), ("gamma", 10)] {
        std::fs::write(tmp.path().join(n), "x".repeat(size)).unwrap();
    }
    let mut app = two_panels(tmp.path(), tmp.path());
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    let shown = |app: &App| match &app.mode {
        Mode::Stats(st) => st
            .shown()
            .map(|n| n.name.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        _ => panic!("not in stats"),
    };
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "AM");
    assert_eq!(shown(&app), ["gamma"]);
    key(&mut app, KeyCode::Enter);
    // delete acts on the shown row, not on the first child
    key(&mut app, KeyCode::F(8));
    key(&mut app, KeyCode::Char('p'));
    app.tick();
    assert!(!tmp.path().join("gamma").exists() && tmp.path().join("Alpha").exists());
    assert!(shown(&app).is_empty());
    key(&mut app, KeyCode::Esc);
    assert_eq!(shown(&app), ["Alpha", "beta"]);
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn slash_filters_bookmarks_and_enter_goes_to_the_shown_one() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("one"), tmp.path().join("two"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    app.hotlist = vec![hotlist::Bookmark::new(a), hotlist::Bookmark::new(b.clone())];
    with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "TW");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panels[app.active].path(), b);
}

#[test]
fn stats_info_popup_and_back() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("f"), "abc").unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    key(&mut app, KeyCode::Char('i'));
    let Mode::Message(text) = &app.mode else {
        panic!("no popup")
    };
    assert!(
        text.contains(&*tmp.path().join("f").to_string_lossy()) && text.contains("Files:       1")
    );
    key(&mut app, KeyCode::Char('x'));
    app.tick();
    assert!(matches!(app.mode, Mode::Stats(_)));
}

#[test]
fn stats_sorts_toggles_count_and_browses() {
    let tmp = tempfile::tempdir().unwrap();
    let many = tmp.path().join("many");
    std::fs::create_dir(&many).unwrap();
    for n in ["a", "b", "c"] {
        std::fs::write(many.join(n), "x").unwrap();
    }
    std::fs::write(tmp.path().join("big"), "x".repeat(100)).unwrap();
    let mut app = two_panels(tmp.path(), tmp.path());
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    let top = |app: &App| match &app.mode {
        Mode::Stats(st) => st.current().children[st.sel].name.clone(),
        _ => panic!("not in stats"),
    };
    assert_eq!(top(&app), "big");
    key(&mut app, KeyCode::Tab); // only the shown value changes
    assert_eq!(top(&app), "big");
    key(&mut app, KeyCode::Char('s')); // active key again: smallest first
    assert_eq!(top(&app), "big"); // cursor stays on its entry
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert_eq!(st.current().children[0].name, "many");
    key(&mut app, KeyCode::Char('C'));
    assert_eq!(top(&app), "big");
    key(&mut app, KeyCode::Home);
    assert_eq!(top(&app), "many");
    key(&mut app, KeyCode::Enter);
    assert_eq!(top(&app), "a");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(top(&app), "many");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::F(3));
    assert!(matches!(app.mode, Mode::View { .. }));
    key(&mut app, KeyCode::Esc);
    app.tick();
    assert_eq!(top(&app), "a"); // back in stats after the viewer
    key(&mut app, KeyCode::F(10));
    assert!(matches!(app.mode, Mode::Normal) && !app.quit);
}

#[test]
fn quit_asks_first() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::F(10));
    assert!(matches!(app.mode, Mode::AskQuit) && !app.quit);
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal) && !app.quit);
    key(&mut app, KeyCode::Char('q'));
    key(&mut app, KeyCode::Enter);
    assert!(app.quit);
}

#[test]
fn footer_follows_screen_and_f10_closes_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    assert_eq!(app.footer_keys(), &FOOTER);
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(matches!(app.mode, Mode::FindForm(_)));
    assert_eq!(app.footer_keys(), &FIND_FORM_FOOTER);
    key(&mut app, KeyCode::F(10));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn f1_help_opens_on_forms_and_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    for (code, mods) in [
        (KeyCode::Char('f'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::CONTROL),
        (KeyCode::Char('n'), KeyModifiers::CONTROL),
        (KeyCode::F(1), KeyModifiers::ALT),
    ] {
        app.on_key(KeyEvent::new(code, mods));
        let title = fmt::help(&app.mode).0;
        key(&mut app, KeyCode::F(1));
        assert!(app.help && title != "Help", "{title} {code:?}");
        key(&mut app, KeyCode::Esc);
        assert!(!app.help && !matches!(app.mode, Mode::Normal));
        key(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Normal));
    }
}

#[test]
fn footer_keys_are_documented_in_their_help() {
    let screens: [(&Footer, &[&str]); 9] = [
        (&FOOTER, &[fmt::HELP_MAIN]),
        (&VIEW_FOOTER, &[fmt::HELP_VIEW]),
        (&EDIT_FOOTER, &[fmt::HELP_EDIT]),
        (&FOUND_FOOTER, &[fmt::HELP_FOUND]),
        (&SYNC_FOOTER, &[fmt::HELP_SYNC]),
        (&FIND_FORM_FOOTER, &[fmt::HELP_FIND_FORM]),
        (&FORM_FOOTER, &[fmt::HELP_RENAME, fmt::HELP_SYNC_FORM]),
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
fn f9_menu_runs_the_chosen_action() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.on_key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Menu(..)));
    let i = MENUS[0]
        .1
        .iter()
        .position(|m| m.1 == KeyCode::F(7) && m.2 == N)
        .unwrap();
    for _ in 0..i {
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode, Mode::Mkdir(_)));

    // click the "Go" title, then its first entry (home directory)
    app.mode = Mode::Normal;
    let click = |col, row| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(click(menu_x(1), 0));
    assert!(matches!(app.mode, Mode::Menu(1, _)));
    app.on_mouse(click(menu_x(1) + 2, 2));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn help_line_click_runs_action() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    key(&mut app, KeyCode::F(1));
    let line = fmt::HELP_MAIN
        .lines()
        .position(|l| l.starts_with("Ctrl+B "))
        .unwrap();
    app.help_click(line);
    assert!(!app.help && matches!(app.mode, Mode::Hotlist(_)));
}

#[test]
fn f1_help_esc_closes_typing_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::F(1));
    assert!(app.help);
    for c in "ncdu".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    let e = app.help_entries();
    // section title, the entry and its continuation line
    assert_eq!(e[0].0, "== Highlights ==");
    assert!(e[1].0.contains("FILE STATS") && e[2].0.starts_with(' '));
    key(&mut app, KeyCode::Esc);
    assert!(app.help && app.help_filter.is_empty());
    key(&mut app, KeyCode::Esc);
    assert!(!app.help && matches!(app.mode, Mode::Normal));
}

#[test]
fn help_down_scrolls_without_key_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::F(1));
    type_keys(&mut app, "type to filter");
    assert!(app.help_entries().iter().all(|e| e.1.is_none()));
    key(&mut app, KeyCode::Down);
    assert_eq!(app.help_scroll, 1);
}

#[test]
fn help_filter_and_enter_runs_selected_key() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    key(&mut app, KeyCode::F(1));
    // first key line is selected; Down moves to the next key line
    let first = app.help_sel;
    key(&mut app, KeyCode::Down);
    assert!(app.help_sel > first && app.help_action_at(app.help_sel).is_some());
    key(&mut app, KeyCode::Up);
    assert_eq!(app.help_sel, first);
    // filter to Ctrl+B bookmarks, Enter runs it
    for c in "bookmarks".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert!(app.help_action_at(app.help_sel).is_some());
    key(&mut app, KeyCode::Enter);
    assert!(!app.help && matches!(app.mode, Mode::Hotlist(_)));
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

#[test]
fn hotlist_insert_alias_and_reorder() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let mut app = two_panels(&a, &b);
    let file = tmp.path().join("cfg").join("hotlist");
    app.hotlist_file = Some(file.clone());
    with(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert); // no duplicate
    assert_eq!(app.hotlist.len(), 1);
    app.hotlist = vec![hotlist::Bookmark::new(a), hotlist::Bookmark::new(b)];
    key(&mut app, KeyCode::F(4));
    type_keys(&mut app, "Work");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Hotlist(0)));
    let saved = hotlist::load_bookmarks(&file).unwrap();
    assert_eq!(saved[0].alias, "Work");
    assert!(saved[0].label().starts_with("Work  "));
    with(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
    assert!(matches!(app.mode, Mode::Hotlist(1)));
    assert_eq!(hotlist::load_bookmarks(&file).unwrap()[1].alias, "Work");
    // filter active: positions are ambiguous, nothing moves
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "work");
    key(&mut app, KeyCode::Enter);
    with(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
    assert_eq!(app.hotlist[1].alias, "Work");
}

#[test]
fn hotlist_add_jump_delete_and_history() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = tmp.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let mut app = App::new(Panel::open(&sub).unwrap(), Panel::open(tmp.path()).unwrap());
    let file = tmp.path().join("cfg").join("hotlist");
    app.hotlist_file = Some(file.clone());
    let ctrl =
        |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    ctrl(&mut app, 'd');
    ctrl(&mut app, 'd'); // no duplicate
    assert_eq!(
        hotlist::load_bookmarks(&file).unwrap(),
        [hotlist::Bookmark::new(app.panels[0].path().to_owned())]
    );
    key(&mut app, KeyCode::Tab);
    ctrl(&mut app, 'b');
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.panels[1].path(), sub);
    app.on_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(app.panels[1].path(), tmp.path());
    // vanished dir: error popup, panel unchanged
    std::fs::remove_dir(&sub).unwrap();
    ctrl(&mut app, 'b');
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Message(_)));
    assert_eq!(app.panels[1].path(), tmp.path());
    key(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'b');
    key(&mut app, KeyCode::Backspace); // same as Delete
    assert!(app.hotlist.is_empty() && hotlist::load_bookmarks(&file).unwrap().is_empty());
}

#[test]
fn conflict_dialog_overwrite_all_and_rename() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    for f in ["a", "b"] {
        std::fs::write(left.path().join(f), "new").unwrap();
        std::fs::write(right.path().join(f), "old").unwrap();
    }
    let mut app = App::new(
        Panel::open(left.path()).unwrap(),
        Panel::open(right.path()).unwrap(),
    );
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    key(&mut app, KeyCode::Char('r'));
    key(&mut app, KeyCode::Char('2'));
    key(&mut app, KeyCode::Enter); // "a2"
    wait(&mut app);
    key(&mut app, KeyCode::Char('O')); // overwrite all
    wait(&mut app);
    assert!(matches!(app.mode, Mode::Normal));
    let read = |f| std::fs::read_to_string(right.path().join(f)).unwrap();
    assert_eq!(
        (read("a"), read("a2"), read("b")),
        ("old".into(), "new".into(), "new".into())
    );
}

#[test]
fn mkdir_then_copy_to_other_panel() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let mut app = App::new(
        Panel::open(left.path()).unwrap(),
        Panel::open(right.path()).unwrap(),
    );
    key(&mut app, KeyCode::F(7));
    for c in "new".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    assert!(left.path().join("new").is_dir());
    assert_eq!(app.panels[0].current().unwrap().name, "..");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::F(5));
    assert!(matches!(app.mode, Mode::Transfer(Op::Copy, _, _)));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(right.path().join("new").is_dir());
    assert!(matches!(app.mode, Mode::Normal));
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(matches!(app.mode, Mode::Normal)); // existing dir is merged, no conflict
    // copy in the current dir under a new name (relative to the active panel)
    key(&mut app, KeyCode::F(5));
    for _ in 0..right.path().display().to_string().len() + 5 {
        key(&mut app, KeyCode::Backspace);
    }
    for c in "copy".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(left.path().join("copy").is_dir());
}

fn settle(app: &mut App) {
    wait(app);
    while app.panels.iter().any(|p| p.loading().is_some()) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn type_keys(app: &mut App, text: &str) {
    text.chars().for_each(|c| key(app, KeyCode::Char(c)));
}

/// Fills the find form (name, then content regex, which ticks its checkbox) and searches.
fn find_in_form(app: &mut App, mask: &str, content: Option<&str>) {
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    type_keys(app, mask);
    if let Some(re) = content {
        form_focus(app, F_CONTENT);
        type_keys(app, re);
    }
    key(app, KeyCode::Enter);
}

fn form_focus(app: &mut App, label: &str) {
    let Mode::FindForm(f) = &mut app.mode else {
        panic!("no find form");
    };
    f.focus = f.rows.iter().position(|r| r.label() == label).unwrap();
}

fn wait_found(app: &mut App) {
    while matches!(&app.mode, Mode::Found(f) if f.running) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn find_form_bad_field_keeps_form_open() {
    let tmp = tempfile::tempdir().unwrap();
    let p = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(p(), p());
    find_in_form(&mut app, "*", None); // starts a search
    assert!(matches!(app.mode, Mode::Found(_)));
    key(&mut app, KeyCode::Esc); // stop
    while matches!(&app.mode, Mode::Found(f) if f.running) {
        app.tick();
    }
    key(&mut app, KeyCode::Esc); // close
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    type_keys(&mut app, "*");
    form_focus(&mut app, F_MIN);
    type_keys(&mut app, "10X");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(&app.mode, Mode::FindForm(f) if f.error.is_some()));
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
    // Ctrl+F reopens the last form; a typed or pasted regex ticks content search
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    let Mode::FindForm(f) = &mut app.mode else {
        panic!("no find form");
    };
    assert_eq!(
        (f.text(F_NAME), f.text(F_MIN), f.error.as_deref()),
        ("**", "10X", None)
    );
    assert!(!f.checked(F_CONTENT_ON));
    form_focus(&mut app, F_CONTENT);
    app.on_paste("needle".into());
    assert!(
        matches!(&app.mode, Mode::FindForm(f) if f.checked(F_CONTENT_ON) && f.text(F_CONTENT) == "needle")
    );
}

/// File Stats on a remote panel: scan, view, `:` over ssh and permanent delete.
#[test]
fn remote_stats_view_cmd_and_delete() {
    let left = tempfile::tempdir().unwrap();
    let mem = tc_core::vfs::mem::Mem::new();
    mem.dir("/d");
    mem.put("/d/f", b"xyz");
    let mut app = two_panels(left.path(), left.path());
    app.active = 1;
    app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
    settle(&mut app);
    // `:` on the panel runs over ssh in the panel directory
    key(&mut app, KeyCode::Char(':'));
    type_keys(&mut app, "ls");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.run,
        Some(("ls".into(), "/".into(), Some("mem://".into())))
    );
    app.run = None;
    with(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    assert!(matches!(&app.mode, Mode::Stats(st) if (st.root.size, st.root.files) == (3, 1)));
    key(&mut app, KeyCode::Enter); // into "d"
    key(&mut app, KeyCode::F(3));
    settle(&mut app);
    assert!(matches!(&app.mode, Mode::View { title, .. } if title.ends_with("/d/f")));
    key(&mut app, KeyCode::Esc);
    app.tick();
    // F4 edits a temp copy and uploads it on save
    key(&mut app, KeyCode::F(4));
    settle(&mut app);
    assert!(matches!(app.mode, Mode::Edit { .. }));
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('y'));
    wait(&mut app);
    app.tick();
    assert_eq!(mem.get("/d/f").unwrap(), b"xxyz");
    assert!(matches!(app.mode, Mode::Stats(_)) && app.edit_remote.is_none());
    key(&mut app, KeyCode::Char(':'));
    type_keys(&mut app, "ls");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(&app.run, Some((c, _, Some(u))) if c == "ls" && u == "mem://"));
    app.run = None;
    app.tick();
    key(&mut app, KeyCode::F(8));
    assert!(
        matches!(&app.mode, Mode::Stats(st) if st.confirm.as_ref().is_some_and(|t| t.contains("No trash")))
    );
    key(&mut app, KeyCode::Char('y')); // no trash: ignored
    key(&mut app, KeyCode::Char('p'));
    settle(&mut app);
    app.tick();
    assert!(mem.get("/d/f").is_none());
    assert!(
        matches!(&app.mode, Mode::Stats(st) if st.root.size == 0 && st.current().children.is_empty())
    );
    key(&mut app, KeyCode::Esc);
    app.tick();
}

#[test]
fn remote_panel_copy_delete_mkdir_and_blocked_keys() {
    let left = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("a"), "data").unwrap();
    let mem = tc_core::vfs::mem::Mem::new();
    let mut app = two_panels(left.path(), left.path());
    app.panels[1].open_remote(mem.clone(), "/".into()).unwrap();
    settle(&mut app);
    assert_eq!(app.panels[1].title(), "mem:///");
    // upload: the prefilled absolute path is on the other panel's side
    app.panels[0].focus(OsStr::new("a"));
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert_eq!(mem.get("/a").unwrap(), b"data");
    // download it back under a new name
    key(&mut app, KeyCode::Tab);
    app.panels[1].focus(OsStr::new("a"));
    key(&mut app, KeyCode::F(5));
    let Mode::Transfer(_, _, l) = &mut app.mode else {
        panic!("no transfer prompt")
    };
    l.text.push_str(".back");
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert_eq!(std::fs::read(left.path().join("a.back")).unwrap(), b"data");
    // delete: no trash on remote, only `p`
    key(&mut app, KeyCode::F(8));
    assert!(matches!(&app.mode, Mode::Confirm(_, _, t) if t.contains("No trash")));
    key(&mut app, KeyCode::Char('y'));
    assert!(matches!(app.mode, Mode::Confirm(..)));
    key(&mut app, KeyCode::Char('p'));
    settle(&mut app);
    assert!(mem.get("/a").is_none());
    key(&mut app, KeyCode::F(7));
    type_keys(&mut app, "d");
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert_eq!(mem.paths(), ["/", "/d"]);
    with(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert!(matches!(&app.mode, Mode::FindForm(_)));
    key(&mut app, KeyCode::Esc);
    // ".." at the remote root goes back to the local directory
    app.panels[1].set_cursor(0);
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert!(app.panels[1].remote().is_none());
    assert_eq!(app.panels[1].path(), left.path());
}

#[test]
fn connection_list_add_edit_delete() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    assert!(matches!(app.mode, Mode::Connections(0)));
    key(&mut app, KeyCode::Insert);
    type_keys(&mut app, "me@h");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.connections, [PathBuf::from("sftp://me@h")]);
    key(&mut app, KeyCode::F(4));
    type_keys(&mut app, "ost");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.connections, [PathBuf::from("sftp://me@host")]);
    key(&mut app, KeyCode::Insert);
    for _ in 0..7 {
        key(&mut app, KeyCode::Backspace);
    }
    type_keys(&mut app, "nonsense");
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Message(_)));
    assert_eq!(app.connections.len(), 1);
    key(&mut app, KeyCode::Esc);
    with(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Delete);
    assert!(app.connections.is_empty());
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn shift_arrows_and_clicks_select() {
    let tmp = tempfile::tempdir().unwrap();
    // subdir: ".." then loads the small tempdir at once, not the slow-to-read $TMPDIR (macOS)
    let dir = tmp.path().join("sub");
    std::fs::create_dir(&dir).unwrap();
    for n in ["a", "b", "c", "d"] {
        std::fs::write(dir.join(n), "").unwrap();
    }
    let mut app = App::new(Panel::open(&dir).unwrap(), Panel::open(&dir).unwrap());
    app.areas = [Rect::new(0, 0, 40, 10), Rect::new(40, 0, 40, 10)];
    let shift = |code| KeyEvent::new(code, KeyModifiers::SHIFT);
    app.on_key(shift(KeyCode::Down)); // ".." is not selectable
    app.on_key(shift(KeyCode::Down));
    assert_eq!(app.panels[0].targets().len(), 1);
    let click = |app: &mut App, column, row, modifiers| {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers,
        })
    };
    click(&mut app, 45, 3, KeyModifiers::NONE); // row 3 = entry 2 ("b") in right panel
    assert_eq!((app.active, app.panels[1].cursor()), (1, 2));
    click(&mut app, 45, 5, KeyModifiers::SHIFT); // entries 2..=4
    assert_eq!(app.panels[1].targets().len(), 3);
    click(&mut app, 45, 1, KeyModifiers::NONE); // ".." twice quickly: enters the parent
    let before = app.panels[1].path().to_path_buf();
    click(&mut app, 45, 1, KeyModifiers::NONE);
    assert_ne!(app.panels[1].path(), before);
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

fn wait(app: &mut App) {
    while matches!(app.mode, Mode::Work(_)) || matches!(&app.mode, Mode::Xfer(x) if x.ask.is_none())
    {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

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
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::AskExit(_)));
    key(&mut app, KeyCode::Char('y'));
    assert!(matches!(
        app.mode,
        Mode::AskSave {
            step: Step::Eol,
            ..
        }
    ));
    key(&mut app, KeyCode::Char('y')); // to LF
    wait(&mut app);
    assert!(matches!(app.mode, Mode::Normal));
    let text = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
    assert_eq!(text, "xhi\n");
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

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
    key(app, KeyCode::Enter);
}

fn rows(app: &mut App) -> Vec<String> {
    app.mode.editor_mut().unwrap().visible(5, 30)
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
    type_text(&mut app, "ab");
    type_text(&mut app, "X");
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
    type_text(&mut app, "(");
    assert_eq!(
        app.input
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .map(str::is_empty),
        Some(false)
    );
    key(&mut app, KeyCode::Backspace);
    assert!(app.input.as_ref().unwrap().error.is_none());
    type_text(&mut app, "ONE"); // smart case: uppercase is exact, no match
    assert!(app.input.is_none());
    assert_eq!(app.note, "not found");
    key(&mut app, KeyCode::F(3)); // next with the last search
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::F(7));
    type_text(&mut app, "one");
    assert_eq!(app.mode.editor_mut().unwrap().cursor, (0, 0));
    key(&mut app, KeyCode::F(3));
    assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 4));
    key(&mut app, KeyCode::F(3));
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

    key(&mut app, KeyCode::F(4));
    type_text(&mut app, "o(n)e");
    assert!(app.input.is_some());
    type_text(&mut app, "[${1}]");
    assert_eq!(app.note, "2 replaced");
    assert_eq!(rows(&mut app), ["[n]", "two [n]", ""]);

    ctrl(&mut app, 'g');
    type_text(&mut app, "2");
    assert_eq!(app.mode.editor_mut().unwrap().cursor, (1, 0));
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

    alt(&mut app, KeyCode::Char('b'));
    assert!(title(&app).0.ends_with("a.txt"));
    alt(&mut app, KeyCode::Char('f'));
    assert!(title(&app).0.ends_with("c.txt"));
    // Ctrl+H (some terminals' Backspace) is not the hex toggle
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

#[test]
fn wheel_moves_the_cursor_of_the_panel_under_the_pointer_by_3() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..10 {
        std::fs::write(dir.path().join(format!("f{n}")), "x").unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    app.areas = [Rect::new(0, 0, 10, 10), Rect::new(10, 0, 10, 10)];
    let wheel = |app: &mut App, kind, column| {
        app.on_mouse(MouseEvent {
            kind,
            column,
            row: 2,
            modifiers: KeyModifiers::NONE,
        })
    };
    wheel(&mut app, MouseEventKind::ScrollDown, 12);
    assert_eq!((app.panels[0].cursor(), app.panels[1].cursor()), (0, 3));
    wheel(&mut app, MouseEventKind::ScrollUp, 12);
    assert_eq!(app.panels[1].cursor(), 0);
}

#[test]
fn delete_text_lists_the_answers_and_no_trash_on_remote() {
    let local = delete_text("Delete a?".into(), true, false);
    assert!(
        local.contains("recursively")
            && local.ends_with("[y] Trash   [p] Delete permanently   [Esc] Cancel")
    );
    let remote = delete_text("Delete a?".into(), false, true);
    assert!(!remote.contains("[y]") && remote.ends_with("[p] Delete permanently   [Esc] Cancel"));
}

#[test]
fn find_then_panelize_and_back() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();
    std::fs::write(dir.path().join("sub/b.txt"), "world\n").unwrap();
    std::fs::write(dir.path().join("c.rs"), "hello\n").unwrap();
    let mut app = App::new(
        Panel::open(dir.path()).unwrap(),
        Panel::open(dir.path()).unwrap(),
    );
    find_in_form(&mut app, "*.txt", Some("hello"));
    wait_found(&mut app);
    assert!(matches!(&app.mode, Mode::Found(f) if f.list.len() == 1));
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.mode, Mode::Normal) && app.panels[0].is_virtual());
    assert_eq!(app.panels[0].targets(), [dir.path().join("a.txt")]);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Esc); // back to the directory
    assert!(!app.panels[0].is_virtual());
    // Esc cancels a running search, a second Esc closes
    find_in_form(&mut app, "", None);
    key(&mut app, KeyCode::Esc);
    while matches!(&app.mode, Mode::Found(f) if f.running) {
        app.tick();
    }
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn found_sorts_by_size_name_and_reverses() {
    let dir = tempfile::tempdir().unwrap();
    for (n, len) in [("b.txt", 3), ("a.txt", 10), ("c.txt", 1)] {
        std::fs::write(dir.path().join(n), vec![b'x'; len]).unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    find_in_form(&mut app, "*.txt", None);
    wait_found(&mut app);
    let shown = |app: &App| match &app.mode {
        Mode::Found(f) => f
            .shown()
            .map(|h| (h.entry.size, h.entry.name.clone()))
            .collect::<Vec<_>>(),
        _ => panic!("no results"),
    };
    let names = |app: &App| {
        shown(app)
            .iter()
            .map(|(_, n)| {
                Path::new(n)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>()
    };
    with(&mut app, KeyCode::F(6), KeyModifiers::CONTROL);
    assert!(matches!(app.mode, Mode::Found(_)));
    assert_eq!(names(&app), ["c.txt", "b.txt", "a.txt"]);
    key(&mut app, KeyCode::Home); // on c.txt
    with(&mut app, KeyCode::F(6), KeyModifiers::CONTROL);
    assert_eq!(names(&app), ["a.txt", "b.txt", "c.txt"]);
    assert!(
        matches!(&app.mode, Mode::Found(f) if f.sel == 2 && fmt::found_title(f).contains("[size desc]"))
    );
    with(&mut app, KeyCode::F(4), KeyModifiers::CONTROL);
    assert_eq!(names(&app), ["a.txt", "b.txt", "c.txt"]);
    assert!(matches!(&app.mode, Mode::Found(f) if fmt::found_title(f).contains("[name]")));
}

#[test]
fn found_sort_keeps_streamed_hits_sorted() {
    let (tx, rx) = mpsc::channel();
    let mut f = Found {
        list: Vec::new(),
        view: Vec::new(),
        filter: Filter::default(),
        sort: None,
        sel: 0,
        running: true,
        remote: false,
        error: Arc::default(),
        re: None,
        query: find::Query::default(),
        rx,
        cancel: Arc::default(),
    };
    f.set_sort(FoundSort::Size);
    let hit = |size| Hit {
        entry: Entry {
            size,
            name: OsString::from(format!("n{size}")),
            is_dir: false,
            is_symlink: false,
            modified: None,
            link: None,
        },
        line: None,
    };
    for size in [5, 1, 9] {
        tx.send(hit(size)).unwrap();
    }
    f.pump();
    for size in [3, 7, 0] {
        tx.send(hit(size)).unwrap();
    }
    f.pump();
    let sizes: Vec<u64> = f.shown().map(|h| h.entry.size).collect();
    assert_eq!(sizes, [0, 1, 3, 5, 7, 9]);
    assert_eq!(f.shown_len(), 6);
    // A filter keeps the cursor row, not an arbitrary hit of the unsorted view.
    f.sel = 0;
    f.filter.line = LineEdit::from("n".to_owned());
    f.rebuild();
    assert_eq!(
        (f.sel, f.shown().next().map(|h| h.entry.size)),
        (0, Some(0))
    );
}

#[test]
fn found_ctrl_r_searches_again_and_slash_filters() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    std::fs::write(dir.path().join("B.txt"), "x").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    find_in_form(&mut app, "*.txt", None);
    wait_found(&mut app);
    std::fs::write(dir.path().join("c.txt"), "x").unwrap();
    with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    wait_found(&mut app);
    let n = |app: &App| match &app.mode {
        Mode::Found(f) => (f.list.len(), f.shown_len()),
        _ => panic!("no results"),
    };
    assert_eq!(n(&app), (3, 3));
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "b.");
    assert_eq!(n(&app), (3, 1));
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter); // panelizes the shown result only
    assert_eq!(app.panels[0].entries().len(), 1);
}

#[test]
fn found_enter_with_empty_filter_keeps_results() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    find_in_form(&mut app, "*.txt", None);
    wait_found(&mut app);
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, "zzz");
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Enter);
    assert!(matches!(&app.mode, Mode::Found(f) if f.shown_len() == 0));
}

#[test]
fn q_and_f10_close_found_results_and_the_menu() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    find_in_form(&mut app, "*", None);
    wait_found(&mut app);
    key(&mut app, KeyCode::Char('q'));
    assert!(matches!(app.mode, Mode::Normal));
    key(&mut app, KeyCode::F(9));
    key(&mut app, KeyCode::F(10));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn stats_ctrl_r_rescans_subtree_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/x"), "123").unwrap();
    std::fs::write(tmp.path().join("y"), "12345").unwrap();
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Down); // sub (3 bytes) below y
    key(&mut app, KeyCode::Enter);
    std::fs::write(tmp.path().join("sub/z"), "x".repeat(10)).unwrap();
    // Esc cancels and keeps the old tree
    with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Esc);
    assert!(matches!(&app.mode, Mode::Stats(st) if !st.scanning() && st.root.size == 8));
    with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert_eq!((st.current().files, st.current().size), (2, 13));
    assert_eq!((st.root.files, st.root.size), (3, 18));
    key(&mut app, KeyCode::Backspace);
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert!(st.current().children.iter().any(|n| n.name == "y"));
}

#[test]
fn stats_ctrl_r_after_shrink_keeps_cursor_valid() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a"), "1234567").unwrap();
    std::fs::write(tmp.path().join("b"), "12").unwrap();
    let mut app = stats_app(tmp.path());
    key(&mut app, KeyCode::Down); // b, the last row
    std::fs::remove_file(tmp.path().join("a")).unwrap();
    with(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
    while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
        app.tick();
    }
    let Mode::Stats(st) = &app.mode else { panic!() };
    assert_eq!((st.current().children.len(), st.root.size), (1, 2));
    assert_eq!(st.selected().map(|n| n.name.clone()), Some("b".into()));
}

#[test]
fn found_lines_open_at_match_and_feed_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir(d.join("sub")).unwrap();
    std::fs::write(d.join("sub/b.txt"), "x\nhello one\ny\nhello two\n").unwrap();
    let mut app = two_panels(d, d);
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    form_focus(&mut app, F_EVERY);
    key(&mut app, KeyCode::Char(' '));
    form_focus(&mut app, F_CONTENT);
    type_keys(&mut app, "hello");
    key(&mut app, KeyCode::Enter);
    wait_found(&mut app);
    let Mode::Found(f) = &app.mode else {
        panic!("no results");
    };
    let lines: Vec<_> = f.list.iter().map(|h| h.line.clone().unwrap().0).collect();
    assert_eq!(lines, [2, 4]);
    assert!(fmt::found_row(&f.list[1]).ends_with("b.txt:4: hello two"));
    // F3 opens the second hit at its line; closing the viewer brings the list back
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::F(3));
    let Mode::View { view, .. } = &mut app.mode else {
        panic!("no viewer");
    };
    assert_eq!(view.lines(1, 80).unwrap(), ["hello two"]);
    key(&mut app, KeyCode::Esc);
    app.tick();
    assert!(matches!(&app.mode, Mode::Found(f) if f.sel == 1));
    // Enter: one panel row per file, shown below the search root; F3 there at the first hit
    key(&mut app, KeyCode::Enter);
    let file = d.join("sub").join("b.txt");
    assert_eq!(app.panels[0].entries().len(), 1);
    assert_eq!(app.panels[0].hit_line(file.as_os_str()), Some(2));
    let today = jiff::ZonedDateTime::now().date();
    let shown = &fmt::entry_cols(&app, 0, &app.panels[0].entries()[0], today)[0];
    assert_eq!(
        shown.trim(),
        Path::new("sub").join("b.txt").to_string_lossy()
    );
    key(&mut app, KeyCode::F(3));
    let Mode::View { view, .. } = &mut app.mode else {
        panic!("no viewer");
    };
    assert_eq!(view.lines(1, 80).unwrap(), ["hello one"]);
    assert!(app.search.as_ref().is_some_and(|re| re.is_match("hello")));
    key(&mut app, KeyCode::Esc);
    app.tick();
    // F4 puts the editor cursor on the hit
    key(&mut app, KeyCode::F(4));
    wait(&mut app);
    assert!(matches!(&app.mode, Mode::Edit { editor, .. } if editor.cursor.0 == 1));
    key(&mut app, KeyCode::Esc);
    // Ctrl+G: the real directory with the cursor on the file
    with(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
    settle(&mut app);
    let p = &app.panels[0];
    assert!(!p.is_virtual() && p.path() == d.join("sub"));
    assert_eq!(p.current().unwrap().name, "b.txt");
}

#[test]
fn rename_presets_save_load_delete() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    let file = dir.path().join("presets");
    app.rename_presets_file = Some(file.clone());
    key(&mut app, KeyCode::Down); // skip ".."
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    for _ in 0..3 {
        key(&mut app, KeyCode::Backspace);
    }
    type_keys(&mut app, "x[N]");
    let preset_row = |app: &mut App| {
        let Mode::MultiRename(f) = &mut app.mode else {
            panic!("no form");
        };
        f.focus = f.rows.len() - 1;
    };
    preset_row(&mut app);
    key(&mut app, KeyCode::F(2)); // no name yet
    assert!(
        matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref().is_some_and(|e| e.contains("name")))
    );
    type_keys(&mut app, "p1");
    key(&mut app, KeyCode::F(2));
    assert!(std::fs::read_to_string(&file).unwrap().starts_with("p1\t"));
    let Mode::MultiRename(f) = &mut app.mode else {
        panic!("no form");
    };
    f.rows = rename_rows();
    key(&mut app, KeyCode::F(3));
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!(
        (form_text(&f.rows, R_NAME), form_text(&f.rows, R_PRESET)),
        ("x[N]", "p1")
    );
    assert_eq!(f.preview[0].1.as_deref(), Ok("xa.txt"));
    key(&mut app, KeyCode::F(8));
    assert!(app.rename_presets.is_empty());
    assert!(std::fs::read_to_string(&file).unwrap().is_empty());
}

#[test]
fn rename_pasted_names() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["a", "b"] {
        std::fs::write(dir.path().join(f), f).unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down); // skip ".."
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert);
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    app.on_paste("x\ny\n".into());
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!(f.preview[1].1.as_deref(), Ok("y"));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(dir.path().join("x").exists() && dir.path().join("y").exists());
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    app.on_paste("z".into()); // one line: into the field
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert!(f.names.is_none() && form_text(&f.rows, R_NAME).ends_with('z'));
    app.on_paste("x\ny\nz".into());
    let Mode::MultiRename(f) = &app.mode else {
        panic!("no form");
    };
    assert!(f.names.is_none() && f.error.as_deref().is_some_and(|e| e.contains("names for")));
}

#[test]
fn rename_f4_edits_names() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["a", "b"] {
        std::fs::write(dir.path().join(f), f).unwrap();
    }
    let mut app = two_panels(dir.path(), dir.path());
    key(&mut app, KeyCode::Down); // skip ".."
    key(&mut app, KeyCode::Insert);
    key(&mut app, KeyCode::Insert);
    with(&mut app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::F(4));
    let names_file = |app: &App| {
        let (_, tmp) = app.rename_back.as_ref().expect("no form stashed");
        tmp.path().join(RENAME_NAMES_FILE)
    };
    assert_eq!(std::fs::read_to_string(names_file(&app)).unwrap(), "a\nb\n");
    assert!(!matches!(app.mode, Mode::MultiRename(_)));
    for (text, ok) in [("p\nq\nr\n", false), ("p\nq\n", true)] {
        let file = names_file(&app);
        std::fs::write(&file, text).unwrap();
        app.mode = Mode::Normal; // the editor closed
        app.tick();
        let Mode::MultiRename(f) = &app.mode else {
            panic!("no form");
        };
        assert_eq!(f.names.is_some(), ok);
        if ok {
            assert_eq!(f.preview[1].1.as_deref(), Ok("q"));
        } else {
            assert!(f.error.as_deref().is_some_and(|e| e.contains("names for")));
            key(&mut app, KeyCode::F(4)); // again, for the good list
        }
    }
}

#[test]
fn rename_ctrl_z_undoes() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["a", "b"] {
        std::fs::write(dir.path().join(f), f).unwrap();
    }
    let other = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), other.path());
    let open = |app: &mut App| {
        key(app, KeyCode::Down); // skip ".."
        key(app, KeyCode::Insert);
        key(app, KeyCode::Insert);
        with(app, KeyCode::Char('m'), KeyModifiers::CONTROL);
    };
    open(&mut app);
    app.on_paste("x\ny\n".into());
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(dir.path().join("x").exists());
    // an unrelated transfer keeps the undo memory
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Enter);
    wait(&mut app);
    assert!(other.path().join("x").exists() || other.path().join("y").exists());
    open(&mut app);
    with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    wait(&mut app);
    assert!(dir.path().join("a").exists() && dir.path().join("b").exists());
    assert!(!dir.path().join("x").exists());
    open(&mut app);
    with(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert!(
        matches!(&app.mode, Mode::MultiRename(f) if f.error.as_deref() == Some("Nothing to undo."))
    );
}

#[test]
fn saved_searches_save_load_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    let file = dir.path().join("searches");
    app.searches_file = Some(file.clone());
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    type_keys(&mut app, "*.rs");
    form_focus(&mut app, F_HIDDEN);
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::F(2)); // no name yet
    assert!(
        matches!(&app.mode, Mode::FindForm(f) if f.error.as_deref().is_some_and(|e| e.contains("name")))
    );
    form_focus(&mut app, F_SAVED);
    type_keys(&mut app, "rust");
    key(&mut app, KeyCode::F(2));
    assert_eq!(cmdline::load_history(&file).unwrap().len(), 1);
    key(&mut app, KeyCode::Esc);
    app.last_find = None; // a fresh form
    app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    key(&mut app, KeyCode::F(3));
    let Mode::FindForm(f) = &app.mode else {
        panic!("no form");
    };
    assert_eq!((f.text(F_NAME), f.text(F_SAVED)), ("*.rs", "rust"));
    assert!(f.checked(F_HIDDEN) && !f.checked(F_FOLLOW));
    key(&mut app, KeyCode::F(8));
    assert!(cmdline::load_history(&file).unwrap().is_empty());
}

#[test]
fn ctrl_v_pastes_changed_host_files_before_app_clip() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("h.txt"), "host\n").unwrap();
    std::fs::write(src.path().join("a.txt"), "app\n").unwrap();
    let mut app = two_panels(src.path(), dst.path());
    key(&mut app, KeyCode::Tab); // paste into the destination panel
    // host files: pasted once, then the app clipboard is used again
    app.paste_with(vec![src.path().join("h.txt")]);
    wait(&mut app);
    assert!(dst.path().join("h.txt").exists());
    app.file_clip = Some(FileClip {
        op: Op::Copy,
        paths: vec![src.path().join("a.txt")],
        side: Side::Local,
    });
    app.paste_with(vec![src.path().join("h.txt")]);
    wait(&mut app);
    assert!(dst.path().join("a.txt").exists());
}

#[test]
fn failed_cut_paste_keeps_the_clip() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let mut app = two_panels(src.path(), dst.path());
    app.file_clip = Some(FileClip {
        op: Op::Move,
        paths: vec![src.path().join("gone.txt")],
        side: Side::Local,
    });
    key(&mut app, KeyCode::Tab);
    with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    wait(&mut app);
    assert!(app.file_clip.is_some() && !dst.path().join("gone.txt").exists());
}

#[test]
fn ctrl_c_v_copies_and_ctrl_x_v_moves_between_panels() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), "one\n").unwrap();
    std::fs::write(src.path().join("b.txt"), "two\n").unwrap();
    let mut app = two_panels(src.path(), dst.path());
    key(&mut app, KeyCode::Down); // a.txt, the first entry after ".."
    with(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Tab); // paste into the other panel
    with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    wait(&mut app);
    assert!(dst.path().join("a.txt").exists() && src.path().join("a.txt").exists());
    // cut b.txt from the left panel and paste it into the right one: it moves
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down); // b.txt
    with(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Tab);
    with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    wait(&mut app);
    assert!(dst.path().join("b.txt").exists() && !src.path().join("b.txt").exists());
    assert!(app.file_clip.is_none());
    // nothing left to paste
    with(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
    wait(&mut app);
    assert!(matches!(&app.mode, Mode::Message(m) if m.starts_with("Nothing copied")));
}

fn procs(app: &App) -> &Procs {
    match &app.mode {
        Mode::Procs(p) => p,
        _ => panic!("not in the process monitor"),
    }
}

/// Tick until the next snapshot arrives.
fn procs_wait(app: &mut App) {
    let before = procs(app).snap.clone();
    let t = std::time::Instant::now();
    while procs(app).snap == before && t.elapsed() < std::time::Duration::from_secs(5) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Ctrl+P: snapshot arrives, sort toggles, filter narrows and clears, Esc closes.
#[test]
fn process_monitor_sort_filter_close() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    while procs(&app).rows.is_empty() {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let all = procs(&app).rows.len();
    assert!(app.idle());
    assert_eq!(app.footer_keys(), &PROCS_FOOTER);
    key(&mut app, KeyCode::Char('p'));
    let p = procs(&app);
    assert_eq!((p.sort, p.rev), (ProcSort::Pid, false));
    let pids: Vec<u32> = p.rows.iter().map(|&i| p.snap.procs[i].pid).collect();
    assert!(pids.is_sorted());
    key(&mut app, KeyCode::Char('p'));
    assert!(procs(&app).rev);
    key(&mut app, KeyCode::Char('/'));
    type_keys(&mut app, &std::process::id().to_string());
    let p = procs(&app);
    assert!(p.filter.editing && p.rows.len() < all);
    assert_eq!(p.current().map(|c| c.pid), Some(std::process::id()));
    key(&mut app, KeyCode::Esc);
    assert_eq!(procs(&app).rows.len(), all);
    // the cursor keeps its row across refreshes
    key(&mut app, KeyCode::Down);
    procs_wait(&mut app);
    assert_eq!(procs(&app).sel, 1);
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(procs(&app).every_edit.as_ref().unwrap().text, "2");
    with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_keys(&mut app, "0,5");
    key(&mut app, KeyCode::Enter);
    assert_eq!(procs(&app).every, std::time::Duration::from_millis(500));
    key(&mut app, KeyCode::Char('s'));
    with(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
    type_keys(&mut app, "0");
    key(&mut app, KeyCode::Enter);
    let p = procs(&app);
    assert_eq!(p.every, std::time::Duration::from_millis(500));
    assert!(p.status.contains("0.5 to 3600"));
    key(&mut app, KeyCode::F(8));
    assert!(procs(&app).confirm.is_some());
    key(&mut app, KeyCode::Char('n'));
    assert!(procs(&app).confirm.is_none());
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.mode, Mode::Normal));
}

/// A connection that cannot run commands (FTP) reports it instead of an empty list.
#[test]
fn process_monitor_needs_commands_on_remote() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = two_panels(dir.path(), dir.path());
    app.panels[0]
        .open_remote(tc_core::vfs::mem::Mem::new(), "/".into())
        .unwrap();
    settle(&mut app);
    with(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    while matches!(app.mode, Mode::Procs(_)) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(&app.mode, Mode::Message(m) if m.contains("no commands")));
}

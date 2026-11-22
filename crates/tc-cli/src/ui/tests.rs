use super::viewer::highlight;
use super::*;
use ratatui::{Terminal, backend::TestBackend};
use tc_core::panel::Panel;

#[test]
fn renders_process_monitor() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.on_key(tc_app::KeyEvent::new(
        tc_app::KeyCode::Char('p'),
        tc_app::KeyModifiers::CONTROL,
    ));
    while matches!(&app.mode, Mode::Procs(p) if p.rows.is_empty()) {
        app.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(screen.contains("Processes: local"));
    assert!(screen.contains("COMMAND"));
    assert!(screen.contains("F8 kill"));
    assert_eq!(app.page, 24 - 1 - 1 - 2 - 2); // status row and F-key bar
}

#[test]
fn renders_panels_and_popup() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("file.txt"), "hi").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.mode = Mode::Mkdir(LineEdit::from("x".to_owned()));
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(screen.contains("file.txt"));
    assert!(screen.contains("Make directory"));
    assert!(screen.contains("F5 Copy"));
}

#[test]
fn renders_editor_with_status() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("e.txt");
    std::fs::write(&path, "hello\nworld").unwrap();
    let editor = tc_core::edit::Editor::open(
        &path,
        &std::sync::atomic::AtomicU64::new(0),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.mode = Mode::Edit {
        editor,
        error: None,
    };
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(screen.contains("world"));
    assert!(screen.contains("1:1  UTF-8  LF"));
}

#[test]
fn viewer_shows_find_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("v.txt"), "hello\n").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.on_key(ratatui::crossterm::event::KeyEvent::from(
        ratatui::crossterm::event::KeyCode::Down,
    ));
    for c in [
        ratatui::crossterm::event::KeyCode::F(3),
        ratatui::crossterm::event::KeyCode::Char('/'),
    ] {
        app.on_key(ratatui::crossterm::event::KeyEvent::from(c));
    }
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(screen.contains("Find (regex)"));
}

#[test]
fn highlights_all_matches() {
    let re = tc_core::search::compile("o").unwrap();
    let line = highlight(
        "foo bar o".into(),
        None,
        &mut false,
        Some(&re),
        None,
        (0, &[]),
    );
    let marked: String = line
        .spans
        .iter()
        .filter(|s| s.style == MARK)
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(marked, "ooo");
    assert_eq!(line.to_string(), "foo bar o");
    // selection overlay and multibyte text do not panic or shift columns
    let line = highlight(
        "é o".into(),
        None,
        &mut false,
        Some(&re),
        Some((0, 2)),
        (0, &[]),
    );
    assert_eq!(line.to_string(), "é o");
}

fn screen(term: &Terminal<TestBackend>) -> String {
    term.backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn status_line_shows_selection_and_link() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a"), "123").unwrap();
    std::fs::write(tmp.path().join("b"), "12345").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    app.panels[0].select_mask("*", true);
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(screen(&term).contains("2 selected, 8 B"));
    app.panels[0].select_mask("*", false);
    app.panels[0].move_cursor(isize::MIN);
    app.panels[0].move_cursor(1);
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(screen(&term).contains("a  3 B  "));
}

#[test]
fn draws_every_screen_on_tiny_terminals() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("e.txt");
    std::fs::write(&file, "hello\nworld").unwrap();
    for (w, h) in [(1, 1), (10, 3), (20, 5), (80, 4)] {
        let open = || Panel::open(tmp.path()).unwrap();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut app = App::new(open(), open());
        term.draw(|f| draw(f, &mut app)).unwrap();
        app.mode = Mode::Mkdir(LineEdit::from("x".to_owned()));
        term.draw(|f| draw(f, &mut app)).unwrap();
        app.mode = Mode::Normal;
        app.help = true;
        term.draw(|f| draw(f, &mut app)).unwrap();
        app.help = false;
        app.mode = Mode::View {
            title: "t".into(),
            view: tc_core::view::Viewer::open(&file).unwrap(),
        };
        app.view_numbers = true;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let editor = tc_core::edit::Editor::open(
            &file,
            &std::sync::atomic::AtomicU64::new(0),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        app.mode = Mode::Edit {
            editor,
            error: None,
        };
        term.draw(|f| draw(f, &mut app)).unwrap();
        app.mode = Mode::Normal;
        let alt_f7 = ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::F(7),
            ratatui::crossterm::event::KeyModifiers::ALT,
        );
        app.on_key(alt_f7);
        for _ in 0..2 {
            app.on_key(ratatui::crossterm::event::KeyEvent::from(
                ratatui::crossterm::event::KeyCode::Enter,
            ));
        }
        term.draw(|f| draw(f, &mut app)).unwrap();
        app.mode = Mode::Normal;
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char(' '),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        term.draw(|f| draw(f, &mut app)).unwrap(); // scanning
        while matches!(&app.mode, Mode::Stats(st) if st.scanning()) {
            app.tick();
        }
        term.draw(|f| draw(f, &mut app)).unwrap();
    }
}

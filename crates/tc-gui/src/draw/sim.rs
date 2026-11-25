use super::{App, Look, Mode, draw};
use crate::keys;
use eframe::egui::{self, Event, Key, Modifiers, RawInput};
use tc_core::panel::Panel;

const SCREEN: egui::Rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(900.0, 500.0));

/// One frame with `events`; returns the texts painted inside the screen.
fn frame(ctx: &egui::Context, app: &mut App, look: &mut Look, events: Vec<Event>) -> String {
    let input = RawInput {
        events: events.clone(),
        screen_rect: Some(SCREEN),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        let field = super::widgets::field_active(ui.ctx());
        for ev in &events {
            if let Some(keys::Input::Key(k)) = keys::translate_for(ev, false, field) {
                app.on_key(k);
            }
        }
        app.tick();
        draw(ui, app, look);
    });
    out.textures_delta.clear();
    let mut texts = String::new();
    for c in out.shapes {
        if let egui::Shape::Text(t) = c.shape
            && SCREEN.contains_rect(t.visual_bounding_rect())
            && c.clip_rect.contains_rect(t.visual_bounding_rect())
        {
            texts += t.galley.text();
            texts.push('\n');
        }
    }
    texts
}

fn press(k: Key, m: Modifiers) -> Event {
    Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: m,
    }
}

#[test]
/// Help table, process table headers and the multi-rename window draw their texts.
fn tables_and_dialogs_draw() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    let mut run = |app: &mut App, ev: Vec<Event>| frame(&ctx, app, &mut look, ev);
    run(&mut app, vec![press(Key::F1, Modifiers::NONE)]);
    let shown = run(&mut app, vec![]);
    assert!(
        shown.contains("Highlights") && shown.contains("Ctrl+B"),
        "{shown}"
    );
    assert!(shown.contains("bookmarks"), "{shown}");
    run(&mut app, vec![press(Key::Escape, Modifiers::NONE)]);
    run(&mut app, vec![press(Key::ArrowDown, Modifiers::NONE)]);
    run(&mut app, vec![press(Key::M, Modifiers::CTRL)]);
    let shown = run(&mut app, vec![]);
    assert!(
        shown.contains("Multi-rename") && shown.contains("a.txt"),
        "{shown}"
    );
    run(&mut app, vec![press(Key::Escape, Modifiers::NONE)]);
    run(&mut app, vec![press(Key::P, Modifiers::CTRL)]);
    let shown = run(&mut app, vec![]);
    assert!(
        shown.contains("PID") && shown.contains("Command"),
        "{shown}"
    );
}

#[test]
/// The process monitor gets keys through the egui event path; its footer replaces the main one.
fn top_keys_in_gui() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::P, Modifiers::CTRL)],
    );
    // first snapshot takes ~0.2 s (CPU % needs two samples)
    while matches!(&app.mode, Mode::Top(p) if p.rows.is_empty()) {
        frame(&ctx, &mut app, &mut look, vec![]);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![Event::Text("s".into()), press(Key::S, Modifiers::NONE)],
    );
    assert!(
        matches!(&app.mode, Mode::Top(p) if p.every_edit.is_some()),
        "s"
    );
    // the prompt and the monitor's own buttons are on screen, not clipped below the rows
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    assert!(shown.contains("update every (seconds)> "), "{shown}");
    assert!(shown.contains("s Interval"), "{shown}");
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::Escape, Modifiers::NONE)],
    );
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::F1, Modifiers::NONE)],
    );
    assert!(app.help, "F1");
    assert_eq!(app.footer_keys()[3].0, "s Interval");
}

/// Screen rect of the first painted text starting with `want`.
fn text_rect(ctx: &egui::Context, app: &mut App, look: &mut Look, want: &str) -> egui::Rect {
    let input = RawInput {
        screen_rect: Some(SCREEN),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| draw(ui, app, look));
    out.textures_delta.clear();
    out.shapes
        .into_iter()
        .find_map(|c| match c.shape {
            egui::Shape::Text(t) if t.galley.text().starts_with(want) => {
                Some(t.galley.rect.translate(t.pos.to_vec2()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{want} not painted"))
}

/// The titles share one vertical center (left- and right-aligned cells mixed).
fn assert_one_line(ctx: &egui::Context, app: &mut App, look: &mut Look, titles: &[&str]) {
    let y = |app: &mut App, look: &mut Look, t| text_rect(ctx, app, look, t).center().y;
    let first = y(app, look, titles[0]);
    for t in &titles[1..] {
        let d = y(app, look, t) - first;
        assert!(d.abs() < 1.0, "{t} is {d} px off {}", titles[0]);
    }
}

#[test]
/// Table header titles of the panels and the process monitor sit on one line.
fn table_headers_share_one_line() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    frame(&ctx, &mut app, &mut look, vec![]);
    assert_one_line(&ctx, &mut app, &mut look, &["Name", "Ext", "Size", "Date"]);
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::P, Modifiers::CTRL)],
    );
    let procs = ["PID", "User", "CPU %", "Memory", "Min", "Command"];
    assert_one_line(&ctx, &mut app, &mut look, &procs);
}

#[test]
/// A click on a sortable process column title sorts by it (shared `head_row`).
fn top_header_click_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    let mut run = |app: &mut App, ev: Vec<Event>| frame(&ctx, app, &mut look, ev);
    run(&mut app, vec![press(Key::P, Modifiers::CTRL)]);
    run(&mut app, vec![]);
    let mut look2 = Look::default();
    let p = text_rect(&ctx, &mut app, &mut look2, "Memory").center();
    let button = |pressed| Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(&mut app, vec![Event::PointerMoved(p)]);
    run(&mut app, vec![button(true)]);
    run(&mut app, vec![button(false)]);
    assert!(matches!(&app.mode, Mode::Top(t) if t.sort == tc_core::top::ProcSort::Mem));
}

#[test]
/// The command line is an egui TextEdit: keys reach the app once, a click moves the app's caret.
fn text_box_keys_once_and_click_moves_caret() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    let run = |app: &mut App, look: &mut Look, ev: Vec<Event>| frame(&ctx, app, look, ev);
    run(&mut app, &mut look, vec![Event::Text(":".into())]);
    let typed = ["a", "b", "c"].map(|t| Event::Text(t.into())).to_vec();
    run(&mut app, &mut look, typed);
    let shown = run(
        &mut app,
        &mut look,
        vec![press(Key::ArrowLeft, Modifiers::NONE)],
    );
    assert!(
        shown.contains("abc") && !shown.contains("abcabc"),
        "{shown}"
    );
    assert!(matches!(&app.mode, Mode::Cmd(l) if l.text == "abc" && l.cur == 2));
    let p = text_rect(&ctx, &mut app, &mut look, "abc").min + egui::vec2(1.0, 4.0);
    let button = |pressed| Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(&mut app, &mut look, vec![Event::PointerMoved(p)]);
    run(&mut app, &mut look, vec![button(true)]);
    run(&mut app, &mut look, vec![button(false)]);
    assert!(matches!(&app.mode, Mode::Cmd(l) if l.text == "abc" && l.cur == 0));
    // typing after the click lands at the new caret
    run(&mut app, &mut look, vec![Event::Text("x".into())]);
    assert!(matches!(&app.mode, Mode::Cmd(l) if l.text == "xabc"));
}

#[test]
/// Tab and a click never give egui focus to dialog widgets: the app alone gets Enter / Space.
fn dialog_buttons_take_no_egui_focus() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    let run = |app: &mut App, look: &mut Look, ev: Vec<Event>| frame(&ctx, app, look, ev);
    run(
        &mut app,
        &mut look,
        vec![press(Key::ArrowDown, Modifiers::NONE)],
    );
    run(&mut app, &mut look, vec![press(Key::F8, Modifiers::NONE)]);
    assert!(matches!(app.mode, Mode::Confirm(..)), "F8 opens a confirm");
    let shown = run(&mut app, &mut look, vec![]);
    run(&mut app, &mut look, vec![press(Key::Tab, Modifiers::NONE)]);
    assert!(
        ctx.memory(eframe::egui::Memory::focused).is_none(),
        "Tab: {shown}"
    );
    let p = text_rect(&ctx, &mut app, &mut look, "Delete").min + egui::vec2(2.0, 4.0);
    let button = |pressed| Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(&mut app, &mut look, vec![Event::PointerMoved(p)]);
    run(&mut app, &mut look, vec![button(true)]);
    run(&mut app, &mut look, vec![button(false)]);
    assert!(
        ctx.memory(eframe::egui::Memory::focused).is_none(),
        "click: {shown}"
    );
}

#[test]
/// The title row with its buttons leaves room for the file list.
fn panel_draws_title_buttons_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    frame(&ctx, &mut app, &mut look, vec![]);
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    assert!(shown.contains("a.txt") && shown.contains("🕓"), "{shown}");
}

#[test]
/// Toolbar and panel button icons exist in `emoji-icon-font` (else they draw as empty boxes).
fn icons_exist_in_icon_font() {
    use eframe::egui::{FontFamily, FontId};
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let only = FontFamily::Name("only".into());
    fonts
        .families
        .insert(only.clone(), vec!["emoji-icon-font".into()]);
    ctx.set_fonts(fonts);
    ctx.run_ui(RawInput::default(), |_| {})
        .textures_delta
        .clear();
    let id = FontId::new(16.0, only);
    let icons = tc_gui::app::TOOLBAR
        .iter()
        .flat_map(|g| g.iter().map(|t| t.0));
    let panel = super::panel::BUTTONS.iter().map(|b| b.0);
    for icon in icons
        .chain(panel)
        .chain(super::dialogs::BM_ICONS)
        .chain([super::dialogs::CLOSE_ICON])
    {
        assert!(ctx.fonts_mut(|f| f.has_glyphs(&id, icon)), "{icon}");
    }
}

#[test]
/// Bookmarks edit page draws every row's alias and path, and its buttons.
fn bookmark_edit_page_draws_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    app.hotlist = vec![tc_core::hotlist::Bookmark {
        alias: "Work".into(),
        path: "/srv/work".into(),
    }];
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::B, Modifiers::CTRL)],
    );
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::F4, Modifiers::NONE)],
    );
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    for t in ["Edit bookmarks", "Work", "/srv/work", "Save", "Cancel"] {
        assert!(shown.contains(t), "{t}: {shown}");
    }
}

#[test]
/// Help window (embedded here, no native windows in tests): key column and many rows shown.
fn help_window_shows_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::F1, Modifiers::NONE)],
    );
    frame(&ctx, &mut app, &mut look, vec![]);
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    assert!(
        shown.contains("Ctrl+B") && shown.contains("Highlights"),
        "{shown}"
    );
    assert!(app.help_rows >= 13, "{}", app.help_rows);
}

#[test]
/// Tool screens own the window: no menu bar, their F-key bar at the bottom.
fn tool_screens_hide_the_menu_bar() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    assert!(frame(&ctx, &mut app, &mut look, vec![]).contains("Select"));
    // cursor from `..` onto a.txt, the file to rename
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::ArrowDown, Modifiers::NONE)],
    );
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::M, Modifiers::CTRL)],
    );
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    assert!(
        shown.contains("F10 Rename") && shown.contains("F9 Cancel") && shown.contains("a.txt"),
        "{shown}"
    );
    assert!(!shown.contains("Select"), "{shown}");
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::Escape, Modifiers::NONE)],
    );
    frame(
        &ctx,
        &mut app,
        &mut look,
        vec![press(Key::P, Modifiers::CTRL)],
    );
    let shown = frame(&ctx, &mut app, &mut look, vec![]);
    assert!(
        shown.contains("PID") && !shown.contains("Select"),
        "{shown}"
    );
}

#[test]
/// All scroll areas (panels, help window, lists) stop at the first / last row: no bounce.
fn scrolling_has_no_rubber_band() {
    let ctx = egui::Context::default();
    super::setup_fonts(&ctx);
    let k = ctx.global_style().spacing.scroll.kinetic;
    assert!(!k.rubber_band && k.decay_time == 0.0);
}

#[test]
/// End and the wheel both reach the help's last line (the scrollbar at the bottom too).
fn help_scrolls_to_the_last_line() {
    let tmp = tempfile::tempdir().unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let last = tc_gui::fmt::HELP_MAIN
        .lines()
        .last()
        .unwrap()
        .trim()
        .to_owned();
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Page,
        delta: egui::vec2(0.0, -100.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    };
    for by_wheel in [false, true] {
        let mut app = App::new(open(), open());
        let (ctx, mut look) = (egui::Context::default(), Look::default());
        frame(
            &ctx,
            &mut app,
            &mut look,
            vec![press(Key::F1, Modifiers::NONE)],
        );
        frame(&ctx, &mut app, &mut look, vec![]);
        if by_wheel {
            let at = Event::PointerMoved(egui::pos2(450.0, 300.0));
            frame(&ctx, &mut app, &mut look, vec![at, wheel.clone()]);
        } else {
            app.help_key(tc_gui::KeyEvent::new(
                tc_gui::KeyCode::End,
                tc_gui::KeyModifiers::NONE,
            ));
        }
        for _ in 0..3 {
            frame(&ctx, &mut app, &mut look, vec![]);
        }
        // shown inside the table's clip rect; the embedded window may reach below SCREEN
        let input = RawInput {
            screen_rect: Some(SCREEN),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| draw(ui, &mut app, &mut look));
        out.textures_delta.clear();
        let visible = out.shapes.iter().any(|c| match &c.shape {
            egui::Shape::Text(t) => {
                t.galley.text() == last.trim_start_matches("F10").trim()
                    && c.clip_rect.contains_rect(t.visual_bounding_rect())
            }
            _ => false,
        });
        assert!(visible, "wheel {by_wheel}: {last:?} not visible");
    }
}

#[test]
/// The field selects and clipboard-edits itself; the app gets the text and runs its side effects.
fn text_box_selects_and_multi_rename_preview_follows() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    let open = || Panel::open(tmp.path()).unwrap();
    let mut app = App::new(open(), open());
    let (ctx, mut look) = (egui::Context::default(), Look::default());
    let mut run = |app: &mut App, ev: Vec<Event>| frame(&ctx, app, &mut look, ev);
    run(&mut app, vec![press(Key::ArrowDown, Modifiers::NONE)]);
    run(&mut app, vec![press(Key::M, Modifiers::CTRL)]);
    run(&mut app, vec![]);
    // Name field holds "[N]": select all (Ctrl+A), type over it
    // egui-winit sets `command` with Ctrl (Cmd on macOS)
    let select_all = Modifiers {
        ctrl: true,
        command: true,
        ..Modifiers::NONE
    };
    run(&mut app, vec![press(Key::A, select_all)]);
    run(&mut app, vec![Event::Text("x".into())]);
    run(&mut app, vec![]);
    let Mode::MultiRename(f) = &app.mode else {
        panic!("not in multi-rename")
    };
    assert!(
        matches!(&f.preview[0].1, Ok(n) if n == "x.txt"),
        "{:?}",
        f.preview
    );
    // Shift+Left selects the last char, typing replaces it
    run(&mut app, vec![press(Key::ArrowLeft, Modifiers::SHIFT)]);
    run(&mut app, vec![Event::Text("y".into())]);
    run(&mut app, vec![]);
    let Mode::MultiRename(f) = &app.mode else {
        panic!("not in multi-rename")
    };
    assert!(
        matches!(&f.preview[0].1, Ok(n) if n == "y.txt"),
        "{:?}",
        f.preview
    );
}

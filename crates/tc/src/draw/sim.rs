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
        for ev in &events {
            if let Some(keys::Input::Key(k)) = keys::translate(ev, false) {
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
    assert!(shown.contains("bookmarks (hotlist)"), "{shown}");
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
fn procs_keys_in_gui() {
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
    while matches!(&app.mode, Mode::Procs(p) if p.rows.is_empty()) {
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
        matches!(&app.mode, Mode::Procs(p) if p.every_edit.is_some()),
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

/// Screen position of the painted text `want`.
fn text_pos(ctx: &egui::Context, app: &mut App, look: &mut Look, want: &str) -> egui::Pos2 {
    let input = RawInput {
        screen_rect: Some(SCREEN),
        ..Default::default()
    };
    let out = ctx.run_ui(input, |ui| draw(ui, app, look));
    out.shapes
        .into_iter()
        .find_map(|c| match c.shape {
            egui::Shape::Text(t) if t.galley.text() == want => Some(t.pos),
            _ => None,
        })
        .unwrap()
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
    let p = text_pos(&ctx, &mut app, &mut look, "abc") + egui::vec2(1.0, 4.0);
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

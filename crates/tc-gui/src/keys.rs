//! egui input events as the terminal key events `App` handles, so both front ends share all key logic.

use eframe::egui::{Event, Key, Modifiers};
use tc::{KeyCode, KeyEvent, KeyModifiers};

pub enum Input {
    Key(KeyEvent),
    Paste(String),
}

/// `alt`: Alt held this frame; its Text events are dropped, the Key event carries Alt+letter.
pub fn translate(ev: &Event, alt: bool) -> Option<Input> {
    let key = |code, mods| Some(Input::Key(KeyEvent::new(code, mods)));
    match ev {
        Event::Text(t) if !alt => {
            let mut chars = t.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => key(KeyCode::Char(c), KeyModifiers::NONE),
                // IME or dead-key compositions arrive as one string
                _ => Some(Input::Paste(t.clone())),
            }
        }
        Event::Paste(t) => Some(Input::Paste(t.clone())),
        // egui turns these shortcuts into clipboard events; the app binds them as keys
        Event::Copy => key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Event::Cut => key(KeyCode::Char('x'), KeyModifiers::CONTROL),
        Event::Key {
            key: k,
            pressed: true,
            modifiers,
            ..
        } => {
            let mods = mods(*modifiers);
            let code = named(*k, mods).or_else(|| char_key(*k, mods))?;
            key(code, mods)
        }
        _ => None,
    }
}

/// Option+key typed an ASCII symbol this frame (`[`, `@`, `|` on many macOS layouts):
/// the text is the input, not the Alt+key shortcut. Non-ASCII Option text (∫, ç) stays a shortcut.
pub fn option_typed(events: &[Event]) -> bool {
    events.iter().any(|e| {
        let Event::Text(t) = e else { return false };
        let mut c = t.chars();
        matches!((c.next(), c.next()), (Some(c), None) if c.is_ascii_graphic() && !c.is_ascii_alphanumeric())
    })
}

/// Alt+character key press (not a named key like an arrow), dropped when [`option_typed`].
pub fn is_alt_char(ev: &Event) -> bool {
    matches!(ev, Event::Key { key, pressed: true, modifiers, .. }
        if modifiers.alt && named(*key, mods(*modifiers)).is_none())
}

fn mods(m: Modifiers) -> KeyModifiers {
    let mut out = KeyModifiers::NONE;
    if m.shift {
        out |= KeyModifiers::SHIFT;
    }
    if m.ctrl || m.mac_cmd {
        out |= KeyModifiers::CONTROL;
    }
    if m.alt {
        out |= KeyModifiers::ALT;
    }
    out
}

fn named(k: Key, mods: KeyModifiers) -> Option<KeyCode> {
    Some(match k {
        Key::ArrowUp => KeyCode::Up,
        Key::ArrowDown => KeyCode::Down,
        Key::ArrowLeft => KeyCode::Left,
        Key::ArrowRight => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Esc,
        Key::Tab if mods.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        Key::Tab => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Insert => KeyCode::Insert,
        _ => return fkey(k).map(KeyCode::F),
    })
}

fn fkey(k: Key) -> Option<u8> {
    const F: [Key; 12] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    F.iter().position(|&f| f == k).map(|i| i as u8 + 1)
}

/// Ctrl/Alt + printable key: egui sends no Text for those (Alt Text is dropped), so build the char here.
fn char_key(k: Key, mods: KeyModifiers) -> Option<KeyCode> {
    if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    let c = match k {
        Key::Space => ' ',
        Key::Period => '.',
        Key::Equals => '=',
        Key::Plus => '+',
        Key::Minus => '-',
        Key::Backslash => '\\',
        Key::Slash => '/',
        _ => {
            let name = k.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
                _ => return None,
            }
        }
    };
    let shifted = mods.contains(KeyModifiers::SHIFT) && c.is_ascii_alphabetic();
    Some(KeyCode::Char(if shifted {
        c.to_ascii_uppercase()
    } else {
        c
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        Event, Input, Key, KeyCode, KeyModifiers, Modifiers, is_alt_char, option_typed, translate,
    };

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn code(ev: &Event, alt: bool) -> Option<(KeyCode, KeyModifiers)> {
        match translate(ev, alt)? {
            Input::Key(k) => Some((k.code, k.modifiers)),
            Input::Paste(_) => None,
        }
    }

    #[test]
    fn maps_keys_like_the_terminal() {
        let none = Modifiers::NONE;
        let (ctrl, alt, shift) = (Modifiers::CTRL, Modifiers::ALT, Modifiers::SHIFT);
        let cases = [
            (
                press(Key::F5, none),
                Some((KeyCode::F(5), KeyModifiers::NONE)),
            ),
            (
                press(Key::F4, shift),
                Some((KeyCode::F(4), KeyModifiers::SHIFT)),
            ),
            (
                press(Key::Tab, shift),
                Some((KeyCode::BackTab, KeyModifiers::SHIFT)),
            ),
            (
                press(Key::Enter, none),
                Some((KeyCode::Enter, KeyModifiers::NONE)),
            ),
            (
                press(Key::S, ctrl),
                Some((KeyCode::Char('s'), KeyModifiers::CONTROL)),
            ),
            (
                press(Key::Space, ctrl),
                Some((KeyCode::Char(' '), KeyModifiers::CONTROL)),
            ),
            (
                press(Key::Period, alt),
                Some((KeyCode::Char('.'), KeyModifiers::ALT)),
            ),
            (
                press(Key::C, alt | shift),
                Some((KeyCode::Char('C'), KeyModifiers::ALT | KeyModifiers::SHIFT)),
            ),
            // plain letters come as Text, not twice
            (press(Key::A, none), None),
            (
                Event::Text("q".into()),
                Some((KeyCode::Char('q'), KeyModifiers::NONE)),
            ),
            (
                Event::Copy,
                Some((KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ),
        ];
        for (ev, want) in cases {
            assert_eq!(code(&ev, false), want, "{ev:?}");
        }
        assert_eq!(code(&Event::Text("ç".into()), true), None);
        // macOS German layout: Option+5 types "[" (text wins), Option+b types "∫" (Alt+b wins)
        let bracket = [press(Key::Num5, Modifiers::ALT), Event::Text("[".into())];
        assert!(option_typed(&bracket) && is_alt_char(&bracket[0]));
        assert!(!option_typed(&[Event::Text("∫".into())]));
        assert!(!option_typed(&[Event::Text("a".into())]));
        assert!(!is_alt_char(&press(Key::ArrowLeft, Modifiers::ALT)));
        assert!(matches!(
            translate(&Event::Paste("a\nb".into()), false),
            Some(Input::Paste(t)) if t == "a\nb"
        ));
    }
}

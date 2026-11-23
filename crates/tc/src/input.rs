//! Key input `App` handles; the desktop app converts egui events to these (`keys.rs`).

use std::ops::{BitOr, BitOrAssign};

/// A key, without modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Backspace,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Delete,
    Insert,
    F(u8),
    Char(char),
    /// Never typed: actions without a shortcut (menu entries, F1 help) send it.
    Null,
    Esc,
}

/// Set of held modifier keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const CONTROL: Self = Self(2);
    pub const ALT: Self = Self(4);

    /// All of `other` are held.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Both sets (const `|`).
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Any of `other` is held.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// A key press with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

#[cfg(test)]
mod tests {
    use super::KeyModifiers;

    #[test]
    fn modifiers_combine() {
        let m = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(m.contains(KeyModifiers::SHIFT) && !m.contains(KeyModifiers::ALT));
        assert!(m.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL));
        assert!(!KeyModifiers::NONE.intersects(m) && m.contains(KeyModifiers::NONE));
    }
}

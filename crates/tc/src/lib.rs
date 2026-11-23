//! App state and logic for the `tc` desktop app.

pub mod app;
pub mod fmt;

// front ends feed `App` terminal-style input; the desktop app converts its events to these
pub use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
pub use ratatui::layout::Rect;

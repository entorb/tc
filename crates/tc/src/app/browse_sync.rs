//! Synchronous browsing (Ctrl+K): entering a directory or going up in the active panel repeats it in the other.

use super::{App, Mode, Panel};
use std::ffi::OsString;

/// One navigation step of the active panel, read before its (async) load replaces the data.
pub(super) enum Step {
    Enter(OsString),
    Parent,
}

/// The step an Enter / Backspace on `panel` is about to take; `None` if the path will not change.
pub(super) fn step(panel: &Panel, enter: bool) -> Option<Step> {
    if panel.is_virtual() || (!enter && panel.path().parent().is_none()) {
        return None;
    }
    if !enter {
        return Some(Step::Parent);
    }
    let e = panel.current().filter(|e| e.is_dir)?;
    Some(if e.is_parent() {
        Step::Parent
    } else {
        Step::Enter(e.name.clone())
    })
}

fn plain(p: &Panel) -> bool {
    p.remote().is_none() && p.archive().is_none() && !p.is_virtual()
}

impl App {
    /// Ctrl+K: toggle; the panel titles show `[sync]` while on.
    pub(super) fn toggle_browse_sync(&mut self) {
        self.sync_browse = !self.sync_browse;
    }

    /// Repeat `step` of the active panel in the other one; if it cannot follow, sync browsing goes off.
    pub(super) fn browse_follow(&mut self, step: Step) {
        let other = 1 - self.active;
        let res = if !plain(&self.panels[self.active]) || !plain(&self.panels[other]) {
            Err("local directories only".to_owned())
        } else {
            let o = &mut self.panels[other];
            match step {
                Step::Enter(name) => {
                    let target = o.path().join(&name);
                    if target.is_dir() {
                        o.cd(target).map_err(|e| e.to_string())
                    } else {
                        Err(format!("no {} in the other panel", name.to_string_lossy()))
                    }
                }
                Step::Parent if o.path().parent().is_none() => {
                    Err("other panel is at the root".to_owned())
                }
                Step::Parent => o.parent().map_err(|e| e.to_string()),
            }
        };
        if let Err(why) = res {
            self.sync_browse = false;
            self.mode = Mode::Message(format!("Sync browsing off: {why}"));
        }
    }
}

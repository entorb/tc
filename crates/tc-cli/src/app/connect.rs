//! Remote sessions: disconnect.

use tc_core::vfs::Side;

use super::{App, Mode};

impl App {
    /// Panels and tabs on a remote go back to their local directory; remembered remote
    /// clipboard, undo and sync state is dropped, so each session closes with its last user.
    pub(super) fn disconnect_all(&mut self) {
        let remote = |s: &Side| matches!(s, Side::Remote(_));
        let panels = self.panels.iter_mut().chain(self.tabs.iter_mut().flatten());
        let errors: Vec<String> = panels
            .filter_map(|p| p.disconnect().err())
            .map(|e| e.to_string())
            .collect();
        self.file_clip.take_if(|c| remote(&c.side));
        self.rename_undo.take_if(|b| remote(&b.side));
        self.sync_back.take_if(|s| s.sides.iter().any(remote));
        self.sync_again.take_if(|s| s.1.iter().any(remote));
        self.found.take_if(|f| f.remote);
        if !errors.is_empty() {
            self.mode = Mode::Message(errors.join("\n"));
        }
    }
}

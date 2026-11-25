//! egui rendering of the shared `App` state; texts come from `tc::fmt`, so both front ends say the same.

use eframe::egui::{self, Color32, Context, FontFamily, FontId, Id, Modal, Ui};
use tc::app::{App, Mode};
mod viewer;
use self::viewer::{draw_editor, draw_view};
mod panel;
#[cfg(test)]
mod sim;
use self::panel::draw_panel;
mod lists;
use self::lists::{draw_found, draw_procs, draw_stats, draw_sync, status_line};
mod dialogs;
mod help;
use self::dialogs::overlays;
mod tools;
use self::tools::{draw_find_form, draw_multi_rename};
mod chrome;
mod widgets;
use self::chrome::{cmdline, footer, menu_bar, toolbar};
use self::widgets::apply_field;
pub(crate) use self::widgets::field_active;

const ACCENT: Color32 = Color32::from_rgb(90, 160, 255);
const MARK: Color32 = Color32::from_rgb(255, 200, 60);
const LINK: Color32 = Color32::from_rgb(200, 140, 255);
const TEXT: Color32 = Color32::from_gray(210);
const MUTED: Color32 = Color32::from_gray(130);
const SELECT_BG: Color32 = Color32::from_rgb(40, 70, 120);

fn mono() -> FontId {
    FontId::monospace(14.0)
}

/// Font family that draws every icon from `emoji-icon-font`, so toolbar glyphs share one style.
fn icons() -> FontId {
    FontId::new(16.0, FontFamily::Name("icons".into()))
}

/// Registers the `icons` family: emoji-icon-font first, the default fonts as fallback.
pub fn setup_fonts(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    let family = ["emoji-icon-font", "Hack", "Ubuntu-Light"].map(String::from);
    fonts
        .families
        .insert(FontFamily::Name("icons".into()), family.into());
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        // a file manager, not a document: arrow pointer, no text selection on labels
        s.interaction.selectable_labels = false;
        s.interaction.tooltip_delay = 0.1;
        // every scroll area: no coasting, no rubber-band past the first / last row
        s.spacing.scroll.kinetic.decay_time = 0.0;
        s.spacing.scroll.kinetic.rubber_band = false;
    });
}

/// Modal without the fade-in animation.
fn modal(id: &str) -> Modal {
    let mut m = Modal::new(Id::unique(id));
    m.area = m.area.fade_in(false);
    m
}

/// Frontend state that `App` does not keep: last drawn cursor per panel (scroll only when it moved).
#[derive(Default)]
pub struct Look {
    cursor: [Option<usize>; 2],
}

pub fn draw(ui: &mut Ui, app: &mut App, look: &mut Look) {
    let today = jiff::ZonedDateTime::now().date();
    egui::Panel::bottom("footer").show(ui, |ui| footer(ui, app));
    if matches!(app.mode, Mode::View { .. }) {
        egui::CentralPanel::default().show(ui, |ui| draw_view(ui, app));
    } else if app.mode.editor_mut().is_some() {
        egui::CentralPanel::default().show(ui, |ui| draw_editor(ui, app));
    } else if app.sub_screen() {
        // sub-screens own the window: F-key bar, their status line, no menu or toolbar
        if !matches!(
            app.mode,
            Mode::Found(_) | Mode::MultiRename(_) | Mode::FindForm(_)
        ) {
            egui::Panel::bottom("status").show(ui, |ui| status_line(ui, app));
        }
        egui::CentralPanel::default().show(ui, |ui| match &app.mode {
            Mode::Found(_) => draw_found(ui, app),
            Mode::Sync(_) => draw_sync(ui, app),
            Mode::Procs(_) => draw_procs(ui, app),
            Mode::MultiRename(_) => draw_multi_rename(ui, app),
            Mode::FindForm(_) => draw_find_form(ui, app),
            _ => draw_stats(ui, app),
        });
    } else {
        egui::Panel::top("menu").show(ui, |ui| menu_bar(ui, app));
        egui::Panel::top("toolbar").show(ui, |ui| toolbar(ui, app));
        if matches!(app.mode, Mode::Cmd(_)) {
            egui::Panel::bottom("cmd").show(ui, |ui| cmdline(ui, app));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.columns(2, |cols| {
                for (i, c) in cols.iter_mut().enumerate() {
                    draw_panel(c, app, look, i, today);
                }
            });
        });
    }
    overlays(ui.ctx(), app);
    apply_field(ui.ctx(), app);
}

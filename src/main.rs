//! optination — a cursor theme chooser that shows you the cursors.
//!
//! Replaces a numbered-list shell prompt whose two real failings were that it
//! could not show you what you were picking, and that it only knew about
//! Xcursor themes — leaving every hyprcursor theme on the system invisible.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apply;
mod cli;
mod render;
mod scan;

use std::rc::Rc;

use rayon::prelude::*;
use slint::{Image, Model, ModelRc, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

/// A theme plus its rendered previews, kept on the Rust side so filtering never
/// has to re-decode anything.
struct Entry {
    theme: scan::Theme,
    shapes: usize,
    card: ThemeCard,
}

fn to_image(bitmap: Option<render::Bitmap>) -> Image {
    match bitmap {
        Some(b) => {
            let buffer = SharedPixelBuffer::clone_from_slice(&b.rgba, b.width, b.height);
            if b.premultiplied {
                Image::from_rgba8_premultiplied(buffer)
            } else {
                Image::from_rgba8(buffer)
            }
        }
        // An empty image renders as nothing, which is the honest answer for a
        // shape the theme does not define.
        None => Image::default(),
    }
}

fn build_card(theme: &scan::Theme, shapes: usize, bitmaps: Vec<Option<render::Bitmap>>) -> ThemeCard {
    let present: Vec<bool> = bitmaps.iter().map(Option::is_some).collect();
    let mut previews = bitmaps.into_iter();
    let mut next = || to_image(previews.next().flatten());
    ThemeCard {
        name: theme.name.as_str().into(),
        comment: theme.comment.as_str().into(),
        badge: theme.format.label().into(),
        shapes: shapes as i32,
        user_installed: theme.user_installed,
        mirrored: theme.mirrored,
        tone: theme.tone.label().into(),
        p0: next(),
        p1: next(),
        p2: next(),
        p3: next(),
        p4: next(),
        p5: next(),
        h0: present[0],
        h1: present[1],
        h2: present[2],
        h3: present[3],
        h4: present[4],
        h5: present[5],
    }
}

/// Decode in parallel, build cards serially.
///
/// `slint::Image` is deliberately `!Send` (it can hold a GPU-side handle), so
/// only the pixel decoding — which is the expensive half, ~90 themes of zip
/// inflation and SVG rasterization — crosses threads.
fn render_all(themes: &[scan::Theme], size: u32) -> Vec<Entry> {
    let decoded: Vec<(usize, Vec<Option<render::Bitmap>>)> = themes
        .par_iter()
        .map(|theme| (render::shape_count(theme), render::preview(theme, size)))
        .collect();

    themes
        .iter()
        .zip(decoded)
        .map(|(theme, (shapes, bitmaps))| Entry {
            card: build_card(theme, shapes, bitmaps),
            theme: theme.clone(),
            shapes,
        })
        .collect()
}

/// The chip state, read off the UI in one go.
///
/// Chips within a group are OR-ed and the groups are AND-ed, so "hyprcursor"
/// plus "dark" narrows to dark hyprcursor themes instead of producing the empty
/// set. A group with nothing checked places no constraint at all.
#[derive(Clone, Copy, Default)]
struct Filters {
    x11: bool,
    hypr: bool,
    light: bool,
    dark: bool,
    standard: bool,
    mirrored: bool,
    system: bool,
    user: bool,
}

impl Filters {
    fn read(ui: &AppWindow) -> Self {
        Self {
            x11: ui.get_f_x11(),
            hypr: ui.get_f_hypr(),
            light: ui.get_f_light(),
            dark: ui.get_f_dark(),
            standard: ui.get_f_standard(),
            mirrored: ui.get_f_mirrored(),
            system: ui.get_f_system(),
            user: ui.get_f_user(),
        }
    }

    fn allows(&self, theme: &scan::Theme) -> bool {
        let group = |a: bool, b: bool, is_a: bool, is_b: bool| {
            (!a && !b) || (a && is_a) || (b && is_b)
        };
        group(self.x11, self.hypr, theme.format.has_x11(), theme.format.has_hypr())
            && group(
                self.light,
                self.dark,
                theme.tone == scan::Tone::Light,
                theme.tone == scan::Tone::Dark,
            )
            && group(self.standard, self.mirrored, !theme.mirrored, theme.mirrored)
            && group(self.system, self.user, !theme.user_installed, theme.user_installed)
    }
}

fn matches(entry: &Entry, needle: &str, filters: &Filters) -> bool {
    if !filters.allows(&entry.theme) {
        return false;
    }
    if needle.is_empty() {
        return true;
    }
    let needle = needle.to_lowercase();
    entry.theme.name.to_lowercase().contains(&needle)
        || entry.theme.comment.to_lowercase().contains(&needle)
        || entry.theme.format.label().contains(&needle)
        || entry.theme.tone.label().contains(&needle)
}

fn main() -> Result<(), slint::PlatformError> {
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }

    let themes = scan::themes();
    if themes.is_empty() {
        eprintln!("optination: no cursor themes found under ~/.local/share/icons, ~/.icons, or /usr/share/icons");
        std::process::exit(1);
    }

    let (current_theme, current_size) = apply::current();
    let initial_size = current_size.unwrap_or(24).clamp(16, 64);

    // Captured before anything is applied: clicking a row changes the live
    // pointer immediately, which is the whole point, so there has to be a way
    // back to whatever you walked in with.
    let original = (current_theme.clone(), current_size);

    let ui = AppWindow::new()?;
    ui.set_total(themes.len() as i32);
    ui.set_slot_labels(ModelRc::new(VecModel::from(
        render::SLOTS.iter().map(|s| SharedString::from(s.label)).collect::<Vec<_>>(),
    )));
    ui.set_size(initial_size as f32);
    ui.set_current_theme(current_theme.clone().unwrap_or_default().into());
    ui.set_can_revert(original.0.is_some());
    if let Some(path) = apply::stale_conf() {
        ui.set_stale_conf(path.display().to_string().into());
    }

    // Entries are the full, unfiltered set; the model the UI sees is a
    // projection of it. `entries` is only ever touched on the UI thread.
    let entries = Rc::new(std::cell::RefCell::new(render_all(&themes, initial_size)));
    // Maps a row index in the visible model back into `entries`.
    let visible = Rc::new(std::cell::RefCell::new(Vec::<usize>::new()));
    let model = Rc::new(VecModel::<ThemeCard>::default());
    ui.set_cards(ModelRc::from(model.clone()));

    let refresh = {
        let entries = entries.clone();
        let visible = visible.clone();
        let model = model.clone();
        move |needle: &str, filters: &Filters, select_name: Option<String>| -> Option<i32> {
            let entries = entries.borrow();
            let mut indices = Vec::new();
            let mut cards = Vec::new();
            for (i, entry) in entries.iter().enumerate() {
                if matches(entry, needle, filters) {
                    indices.push(i);
                    cards.push(entry.card.clone());
                }
            }
            let selected = select_name.and_then(|name| {
                indices
                    .iter()
                    .position(|&i| entries[i].theme.name == name)
                    .map(|p| p as i32)
            });
            *visible.borrow_mut() = indices;
            model.set_vec(cards);
            selected
        }
    };

    /// Re-project the entry list through the current search text and chips.
    ///
    /// The selection is pinned to the *theme*, not to a row index, so narrowing
    /// the list never quietly points "Apply" at something else.
    fn reproject(
        ui: &AppWindow,
        refresh: &impl Fn(&str, &Filters, Option<String>) -> Option<i32>,
    ) -> Option<String> {
        let keep = ui
            .get_cards()
            .row_data(ui.get_selected().max(0) as usize)
            .map(|c| c.name.to_string());
        let needle = ui.get_filter().to_string();
        let filters = Filters::read(ui);
        match refresh(&needle, &filters, keep.clone()) {
            Some(row) => ui.set_selected(row),
            None => ui.set_selected(-1),
        }
        keep
    }

    // Preselect whatever is already in use, so the app opens on your own cursor.
    if let Some(row) = refresh("", &Filters::default(), current_theme.clone()) {
        ui.set_selected(row);
    }
    ui.set_status(
        format!(
            "{} themes  ·  click one to try it live  ·  {} hyprcursor themes the old script never listed",
            themes.len(),
            entries
                .borrow()
                .iter()
                .filter(|e| e.theme.format == scan::Format::Hyprcursor)
                .count()
        )
        .into(),
    );

    let theme_at = {
        let entries = entries.clone();
        let visible = visible.clone();
        move |row: i32| -> Option<(String, usize)> {
            let row = usize::try_from(row).ok()?;
            let index = *visible.borrow().get(row)?;
            let entries = entries.borrow();
            let entry = entries.get(index)?;
            Some((entry.theme.name.clone(), entry.shapes))
        }
    };

    // ------------------------------------------------------------ actions ---

    ui.on_filter_changed({
        let ui = ui.as_weak();
        let refresh = refresh.clone();
        move |_needle: SharedString| {
            reproject(&ui.unwrap(), &refresh);
        }
    });

    ui.on_filters_changed({
        let ui = ui.as_weak();
        let refresh = refresh.clone();
        move || {
            reproject(&ui.unwrap(), &refresh);
        }
    });

    ui.on_size_changed({
        let ui = ui.as_weak();
        let entries = entries.clone();
        let themes = themes.clone();
        let refresh = refresh.clone();
        move |size: i32| {
            let ui = ui.unwrap();
            let size = size.clamp(1, 256) as u32;
            // Every preview is decoded at the requested size, so the slider
            // means a full re-render, not a rescale of what is on screen.
            *entries.borrow_mut() = render_all(&themes, size);
            let keep = reproject(&ui, &refresh);
            // Re-apply at the new size so the live pointer tracks the slider.
            if let Some(name) = keep {
                let _ = apply::live(&name, size);
                let _ = apply::session(&name, size);
                ui.set_status(format!("{name} @ {size}px — live").into());
                ui.set_status_is_error(false);
            }
        }
    });

    ui.on_apply_live({
        let ui = ui.as_weak();
        let theme_at = theme_at.clone();
        move |row: i32, size: i32| {
            let ui = ui.unwrap();
            let Some((name, shapes)) = theme_at(row) else { return };
            let size = size.clamp(1, 256) as u32;
            let result = apply::live(&name, size).and_then(|_| apply::session(&name, size));
            match result {
                Ok(()) => {
                    ui.set_current_theme(name.as_str().into());
                    ui.set_status_is_error(false);
                    ui.set_status(
                        format!("{name} @ {size}px — live, {shapes} shapes. Not saved yet.").into(),
                    );
                }
                Err(e) => {
                    ui.set_status_is_error(true);
                    ui.set_status(e.into());
                }
            }
        }
    });

    ui.on_revert({
        let ui = ui.as_weak();
        let original = original.clone();
        move || {
            let ui = ui.unwrap();
            let Some(name) = original.0.clone() else {
                ui.set_status_is_error(true);
                ui.set_status("nothing to revert to — no cursor theme was set at startup".into());
                return;
            };
            let size = original.1.unwrap_or(24);
            match apply::live(&name, size).and_then(|_| apply::session(&name, size)) {
                Ok(()) => {
                    ui.set_current_theme(name.as_str().into());
                    ui.set_size(size as f32);
                    ui.set_status_is_error(false);
                    ui.set_status(format!("reverted to {name} @ {size}px").into());
                }
                Err(e) => {
                    ui.set_status_is_error(true);
                    ui.set_status(e.into());
                }
            }
        }
    });

    ui.on_persist({
        let ui = ui.as_weak();
        let theme_at = theme_at.clone();
        move |row: i32, size: i32| {
            let ui = ui.unwrap();
            let Some((name, _)) = theme_at(row) else { return };
            let size = size.clamp(1, 256) as u32;
            let _ = apply::live(&name, size);
            let _ = apply::session(&name, size);
            match apply::persist(&name, size) {
                Ok(path) => {
                    ui.set_current_theme(name.as_str().into());
                    ui.set_status_is_error(false);
                    ui.set_status(format!("saved {name} @ {size}px to {}", path.display()).into());
                }
                Err(e) => {
                    ui.set_status_is_error(true);
                    ui.set_status(e.into());
                }
            }
        }
    });

    ui.run()
}

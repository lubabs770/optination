//! optination — a cursor theme chooser that shows you the cursors.
//!
//! Replaces a numbered-list shell prompt whose two real failings were that it
//! could not show you what you were picking, and that it only knew about
//! Xcursor themes — leaving every hyprcursor theme on the system invisible.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apply;
mod cli;
mod omarchy;
mod render;
mod scan;

use std::cell::RefCell;
use std::rc::Rc;

use rayon::prelude::*;
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

slint::include_modules!();

/// Size of the small pointer thumbnail in each list row. Fixed, so moving the
/// size slider re-renders one theme rather than all of them.
const TILE_PX: u32 = 30;

/// The slider's range, and the sizes the tick buttons jump to.
const SIZE_MIN: i32 = 16;
const SIZE_MAX: i32 = 96;

/// A theme plus its rendered row thumbnail, kept on the Rust side so filtering
/// never has to re-decode anything.
struct Entry {
    theme: scan::Theme,
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
        // An empty image renders as nothing; the UI draws a dash instead, so a
        // missing shape reads as a fact about the theme rather than a glitch.
        None => Image::default(),
    }
}

/// Decode row thumbnails in parallel, build cards serially.
///
/// `slint::Image` is deliberately `!Send` (it can hold a GPU-side handle), so
/// only the pixel decoding crosses threads.
fn build_entries(themes: &[scan::Theme]) -> Vec<Entry> {
    let decoded: Vec<(usize, Option<render::Bitmap>)> = themes
        .par_iter()
        .map(|theme| {
            let shapes = render::shape_count(theme);
            (shapes, render::shape(theme, &render::SLOTS[0], TILE_PX))
        })
        .collect();

    themes
        .iter()
        .zip(decoded)
        .map(|(theme, (shapes, tile))| {
            let mut meta = format!("{} · {shapes} shapes", theme.format.label());
            if theme.tone != scan::Tone::Unstated {
                meta.push_str(&format!(" · {}", theme.tone.label()));
            }
            if theme.mirrored {
                meta.push_str(" · mirrored");
            }
            Entry {
                card: ThemeCard {
                    name: theme.name.as_str().into(),
                    meta: meta.into(),
                    has_tile: tile.is_some(),
                    tile: to_image(tile),
                },
                theme: theme.clone(),
            }
        })
        .collect()
}

/// The dot grid behind the live preview.
///
/// Slint has no tiling image fit, so this is drawn once at a size that covers
/// the preview surface and placed unscaled.
fn dot_grid(dark: bool) -> Image {
    const W: u32 = 900;
    const H: u32 = 700;
    const SPACING: u32 = 16;

    let (r, g, b, a) = if dark { (255, 255, 255, 28) } else { (20, 14, 10, 33) };
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(W, H);
    let width = buffer.width();
    for (i, px) in buffer.make_mut_slice().iter_mut().enumerate() {
        let x = i as u32 % width;
        let y = i as u32 / width;
        *px = if x % SPACING == 1 && y % SPACING == 1 {
            Rgba8Pixel { r, g, b, a }
        } else {
            Rgba8Pixel { r: 0, g: 0, b: 0, a: 0 }
        };
    }
    Image::from_rgba8(buffer)
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
        let group =
            |a: bool, b: bool, is_a: bool, is_b: bool| (!a && !b) || (a && is_a) || (b && is_b);
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

/// The preview draws the pointer larger than the rest, per the design.
fn slot_target(slot: usize, size: i32) -> u32 {
    let scaled = if slot == 0 { f64::from(size) * 1.9 } else { f64::from(size) * 1.25 };
    scaled.round().max(1.0) as u32
}

/// Render the six shapes of one theme into the right-hand pane.
fn show_preview(ui: &AppWindow, theme: &scan::Theme, size: i32) {
    let shapes: Vec<Option<render::Bitmap>> = render::SLOTS
        .iter()
        .enumerate()
        .map(|(i, slot)| render::shape(theme, slot, slot_target(i, size)))
        .collect();
    let present: Vec<bool> = shapes.iter().map(Option::is_some).collect();
    let mut images = shapes.into_iter().map(to_image);

    ui.set_sel_h0(present[0]);
    ui.set_sel_h1(present[1]);
    ui.set_sel_h2(present[2]);
    ui.set_sel_h3(present[3]);
    ui.set_sel_h4(present[4]);
    ui.set_sel_h5(present[5]);

    ui.set_sel_p0(images.next().unwrap_or_default());
    ui.set_sel_p1(images.next().unwrap_or_default());
    ui.set_sel_p2(images.next().unwrap_or_default());
    ui.set_sel_p3(images.next().unwrap_or_default());
    ui.set_sel_p4(images.next().unwrap_or_default());
    ui.set_sel_p5(images.next().unwrap_or_default());

    ui.set_sel_name(theme.name.as_str().into());
    ui.set_command(format!("hyprctl setcursor {} {size}", theme.name).into());
}

/// The slider snaps to even values and the tick buttons jump, so this only has
/// to keep a size in range — the +/- buttons step one pixel at a time and must
/// be able to land on an odd one.
fn clamp_size(size: i32) -> i32 {
    size.clamp(SIZE_MIN, SIZE_MAX)
}

fn main() -> Result<(), slint::PlatformError> {
    if let Some(code) = cli::run() {
        std::process::exit(code);
    }

    let themes = scan::themes();
    if themes.is_empty() {
        eprintln!(
            "optination: no cursor themes found under ~/.local/share/icons, ~/.icons, or /usr/share/icons"
        );
        std::process::exit(1);
    }

    let (current_theme, current_size) = apply::current();
    let initial_size = clamp_size(current_size.map_or(24, |s| s as i32));

    // Captured before anything is applied: clicking a row changes the live
    // pointer immediately, which is the whole point, so there has to be a way
    // back to whatever you walked in with.
    let original = (current_theme.clone(), current_size);

    let ui = AppWindow::new()?;
    ui.set_total(themes.len() as i32);
    ui.set_size(initial_size as f32);
    ui.set_can_revert(original.0.is_some());
    if let Some(path) = apply::stale_conf() {
        ui.set_stale_conf(path.display().to_string().into());
    }

    let grids = (dot_grid(false), dot_grid(true));
    ui.set_dots(grids.1.clone());

    let entries = Rc::new(RefCell::new(build_entries(&themes)));
    // Maps a row index in the visible model back into `entries`.
    let visible = Rc::new(RefCell::new(Vec::<usize>::new()));
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
                indices.iter().position(|&i| entries[i].theme.name == name).map(|p| p as i32)
            });
            *visible.borrow_mut() = indices;
            model.set_vec(cards);
            selected
        }
    };

    // Re-project the entry list through the current search text and chips. The
    // selection is pinned to the *theme*, not to a row index, so narrowing the
    // list never quietly points Apply at something else.
    let reproject = {
        let refresh = refresh.clone();
        move |ui: &AppWindow| -> Option<String> {
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
    };

    let theme_at = {
        let entries = entries.clone();
        let visible = visible.clone();
        move |row: i32| -> Option<scan::Theme> {
            let row = usize::try_from(row).ok()?;
            let index = *visible.borrow().get(row)?;
            entries.borrow().get(index).map(|e| e.theme.clone())
        }
    };

    // Open on whatever cursor is already in use.
    if let Some(row) = refresh("", &Filters::default(), current_theme.clone()) {
        ui.set_selected(row);
        if let Some(theme) = theme_at(row) {
            show_preview(&ui, &theme, initial_size);
        }
    }
    ui.set_status(
        format!(
            "{} themes · {} hyprcursor themes the old script never listed",
            themes.len(),
            entries.borrow().iter().filter(|e| e.theme.format == scan::Format::Hyprcursor).count()
        )
        .into(),
    );

    // ------------------------------------------------------------ actions ---

    ui.on_filter_changed({
        let ui = ui.as_weak();
        let reproject = reproject.clone();
        move |_needle: SharedString| {
            reproject(&ui.unwrap());
        }
    });

    ui.on_filters_changed({
        let ui = ui.as_weak();
        let reproject = reproject.clone();
        move || {
            reproject(&ui.unwrap());
        }
    });

    ui.on_select({
        let ui = ui.as_weak();
        let theme_at = theme_at.clone();
        move |row: i32| {
            let ui = ui.unwrap();
            let Some(theme) = theme_at(row) else { return };
            let size = clamp_size(ui.get_size().round() as i32);
            show_preview(&ui, &theme, size);
            // Applying on click is the point: the real pointer changes under
            // your hand before you commit.
            match apply::live(&theme.name, size as u32)
                .and_then(|()| apply::session(&theme.name, size as u32))
            {
                Ok(()) => {
                    ui.set_status_is_error(false);
                    ui.set_status(format!("{} @ {size}px — live, not saved", theme.name).into());
                }
                Err(e) => {
                    ui.set_status_is_error(true);
                    ui.set_status(e.into());
                }
            }
        }
    });

    ui.on_size_changed({
        let ui = ui.as_weak();
        let theme_at = theme_at.clone();
        move |raw: i32| {
            let ui = ui.unwrap();
            let size = clamp_size(raw);
            ui.set_size(size as f32);
            let Some(theme) = theme_at(ui.get_selected()) else { return };
            show_preview(&ui, &theme, size);
            let _ = apply::live(&theme.name, size as u32);
            let _ = apply::session(&theme.name, size as u32);
            ui.set_status_is_error(false);
            ui.set_status(format!("{} @ {size}px — live, not saved", theme.name).into());
        }
    });

    ui.on_save({
        let ui = ui.as_weak();
        let theme_at = theme_at.clone();
        move || {
            let ui = ui.unwrap();
            let Some(theme) = theme_at(ui.get_selected()) else { return };
            let size = clamp_size(ui.get_size().round() as i32) as u32;
            let _ = apply::live(&theme.name, size);
            let _ = apply::session(&theme.name, size);
            match apply::persist(&theme.name, size) {
                Ok(path) => {
                    ui.set_status_is_error(false);
                    ui.set_status(
                        format!("saved {} @ {size}px to {}", theme.name, path.display()).into(),
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
        let reproject = reproject.clone();
        let theme_at = theme_at.clone();
        move || {
            let ui = ui.unwrap();
            let Some(name) = original.0.clone() else {
                ui.set_status_is_error(true);
                ui.set_status("nothing to revert to — no cursor theme was set at startup".into());
                return;
            };
            let size = original.1.unwrap_or(24);
            match apply::live(&name, size).and_then(|()| apply::session(&name, size)) {
                Ok(()) => {
                    ui.set_size(clamp_size(size as i32) as f32);
                    // Put the selection back on the reverted theme, which may
                    // be filtered out of the visible list right now.
                    ui.set_filter(SharedString::new());
                    reproject(&ui);
                    if let Some(row) = ui
                        .get_cards()
                        .iter()
                        .position(|c| c.name == name)
                        .and_then(|p| i32::try_from(p).ok())
                    {
                        ui.set_selected(row);
                        if let Some(theme) = theme_at(row) {
                            show_preview(&ui, &theme, clamp_size(size as i32));
                        }
                    }
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

    ui.on_mode_changed({
        let ui = ui.as_weak();
        move || {
            let ui = ui.unwrap();
            ui.set_dots(if ui.get_dark() { grids.1.clone() } else { grids.0.clone() });
        }
    });

    ui.on_follow_theme_changed({
        let ui = ui.as_weak();
        move |on: bool| {
            let ui = ui.unwrap();
            if !on {
                return;
            }
            let Some((accent, dark)) = omarchy::accent() else {
                ui.set_follow_theme(false);
                ui.set_status_is_error(true);
                ui.set_status("no Omarchy theme colours found to follow".into());
                return;
            };
            ui.set_accent(accent as i32);
            ui.set_dark(dark);
            ui.invoke_sync_skin();
            ui.invoke_mode_changed();
            ui.set_status_is_error(false);
            ui.set_status("accent following the current Omarchy theme".into());
        }
    });

    ui.run()
}

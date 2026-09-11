// geeq: a minimal geeqie-2020-style image viewer.
// Left panel: folder navigator (top) + file list (bottom).
// Right panel: image view + EXIF metadata + RGB histogram.

use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "ico", "tiff", "tif", "webp", "pnm", "tga", "dds", "hdr",
    "avif", "qoi",
];

fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

struct Histogram {
    r: [u32; 256],
    g: [u32; 256],
    b: [u32; 256],
    max: u32,
}

impl Histogram {
    fn from_rgba(buf: &[u8]) -> Self {
        let mut r = [0u32; 256];
        let mut g = [0u32; 256];
        let mut b = [0u32; 256];

        // Sample rather than scanning every pixel on huge images, for responsiveness.
        let pixel_count = buf.len() / 4;
        let stride: usize = (pixel_count / 500_000).max(1);

        for i in (0..pixel_count).step_by(stride) {
            let px = i * 4;
            r[buf[px] as usize] += 1;
            g[buf[px + 1] as usize] += 1;
            b[buf[px + 2] as usize] += 1;
        }

        let max = r
            .iter()
            .chain(g.iter())
            .chain(b.iter())
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);

        Self { r, g, b, max }
    }
}

/// An image load is split into independent pieces that each arrive on their
/// own schedule: the texture appears as soon as it's decoded rather than
/// waiting on the histogram or EXIF, which are computed in parallel on
/// background threads so the UI thread is never blocked by decoding.
struct LoadedImage {
    path: PathBuf,
    texture: Option<TextureHandle>,
    size: egui::Vec2,
    exif: Option<Vec<(String, String)>>,
    histogram: Option<Histogram>,
}

enum LoadMsg {
    Texture(u64, PathBuf, ColorImage),
    Histogram(u64, PathBuf, Histogram),
    Exif(u64, PathBuf, Vec<(String, String)>),
    Error(u64, String),
}

const NAV_INITIAL_DELAY: f64 = 0.25;
const NAV_REPEAT_INTERVAL: f64 = 0.03;

struct GeeqApp {
    current_dir: PathBuf,
    path_input: String,
    subdirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    selected_file: Option<PathBuf>,
    selected_index: Option<usize>,
    loaded: Option<LoadedImage>,
    error: Option<String>,
    nav_held_key: Option<egui::Key>,
    nav_next_repeat_at: f64,
    show_options: bool,
    show_hidden: bool,
    generation: u64,
    load_tx: mpsc::Sender<LoadMsg>,
    load_rx: mpsc::Receiver<LoadMsg>,
}

impl GeeqApp {
    fn new() -> Self {
        let start_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let (load_tx, load_rx) = mpsc::channel();

        let mut app = Self {
            current_dir: start_dir.clone(),
            path_input: start_dir.display().to_string(),
            subdirs: Vec::new(),
            files: Vec::new(),
            selected_file: None,
            selected_index: None,
            loaded: None,
            error: None,
            nav_held_key: None,
            nav_next_repeat_at: 0.0,
            show_options: false,
            show_hidden: false,
            generation: 0,
            load_tx,
            load_rx,
        };
        app.refresh_dir();
        app
    }

    fn is_hidden(path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
    }

    /// Re-lists the current directory's contents without touching the
    /// current selection (used when toggling the "show hidden" option).
    fn rescan_dir(&mut self) {
        self.subdirs.clear();
        self.files.clear();
        self.error = None;

        let entries = match fs::read_dir(&self.current_dir) {
            Ok(e) => e,
            Err(err) => {
                self.error = Some(format!("Cannot read directory: {err}"));
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !self.show_hidden && Self::is_hidden(&path) {
                continue;
            }
            if path.is_dir() {
                self.subdirs.push(path);
            } else if is_image_file(&path) {
                self.files.push(path);
            }
        }

        self.subdirs
            .sort_by_key(|p| p.file_name().unwrap_or_default().to_ascii_lowercase());
        self.files
            .sort_by_key(|p| p.file_name().unwrap_or_default().to_ascii_lowercase());

        self.path_input = self.current_dir.display().to_string();

        // The previously selected file may have been filtered out (or its
        // position shifted); re-resolve rather than blindly keeping the index.
        self.selected_index = self
            .selected_file
            .as_ref()
            .and_then(|selected| self.files.iter().position(|f| f == selected));
    }

    fn refresh_dir(&mut self) {
        self.selected_index = None;
        self.selected_file = None;
        self.loaded = None;
        self.generation += 1;
        self.rescan_dir();
    }

    fn navigate_to(&mut self, dir: PathBuf) {
        self.current_dir = dir;
        self.refresh_dir();
    }

    fn navigate_up(&mut self) {
        if let Some(parent) = self.current_dir.parent() {
            self.navigate_to(parent.to_path_buf());
        }
    }

    fn select_index(&mut self, index: usize) {
        if let Some(path) = self.files.get(index).cloned() {
            self.selected_index = Some(index);
            self.selected_file = Some(path.clone());
            self.spawn_load(path);
        }
    }

    /// Kicks off decoding on background threads so the UI thread never blocks:
    /// one thread decodes the image and, once it has the pixels in hand,
    /// computes the histogram; a second thread reads EXIF independently and
    /// in parallel. Each piece is sent back as soon as it's ready and tagged
    /// with a generation number so stale results from an image the user has
    /// since navigated away from (e.g. during fast arrow-key scrolling) are
    /// dropped instead of clobbering the current selection.
    fn spawn_load(&mut self, path: PathBuf) {
        self.error = None;
        self.generation += 1;
        let gen = self.generation;
        self.loaded = Some(LoadedImage {
            path: path.clone(),
            texture: None,
            size: egui::Vec2::ZERO,
            exif: None,
            histogram: None,
        });

        let decode_tx = self.load_tx.clone();
        let decode_path = path.clone();
        thread::spawn(move || match decode_to_rgba(&decode_path) {
            Ok(rgba) => {
                let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                let color_image = ColorImage::from_rgba_unmultiplied([w, h], rgba.as_raw());
                let _ = decode_tx.send(LoadMsg::Texture(gen, decode_path.clone(), color_image));
                let histogram = Histogram::from_rgba(rgba.as_raw());
                let _ = decode_tx.send(LoadMsg::Histogram(gen, decode_path, histogram));
            }
            Err(err) => {
                let _ = decode_tx.send(LoadMsg::Error(gen, format!("Failed to open image: {err}")));
            }
        });

        let exif_tx = self.load_tx.clone();
        thread::spawn(move || {
            let exif = read_exif(&path);
            let _ = exif_tx.send(LoadMsg::Exif(gen, path, exif));
        });
    }

    /// Applies any background load results that have arrived since the last
    /// frame. Called every frame; non-blocking.
    fn drain_load_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.load_rx.try_recv() {
            match msg {
                LoadMsg::Texture(gen, path, color_image) => {
                    if gen != self.generation {
                        continue;
                    }
                    if let Some(loaded) = &mut self.loaded {
                        if loaded.path == path {
                            loaded.size = egui::vec2(
                                color_image.size[0] as f32,
                                color_image.size[1] as f32,
                            );
                            loaded.texture = Some(ctx.load_texture(
                                path.display().to_string(),
                                color_image,
                                TextureOptions::LINEAR,
                            ));
                        }
                    }
                }
                LoadMsg::Histogram(gen, path, histogram) => {
                    if gen != self.generation {
                        continue;
                    }
                    if let Some(loaded) = &mut self.loaded {
                        if loaded.path == path {
                            loaded.histogram = Some(histogram);
                        }
                    }
                }
                LoadMsg::Exif(gen, path, exif) => {
                    if gen != self.generation {
                        continue;
                    }
                    if let Some(loaded) = &mut self.loaded {
                        if loaded.path == path {
                            loaded.exif = Some(exif);
                        }
                    }
                }
                LoadMsg::Error(gen, err) => {
                    if gen == self.generation {
                        self.error = Some(err);
                        self.loaded = None;
                    }
                }
            }
        }

        // A load may still be in flight; keep polling so results show up
        // the moment the background threads finish instead of waiting for
        // the next externally-triggered repaint.
        let pending = self
            .loaded
            .as_ref()
            .map(|l| l.texture.is_none() || l.histogram.is_none() || l.exif.is_none())
            .unwrap_or(false);
        if pending {
            ctx.request_repaint();
        }
    }

    fn step_selection(&mut self, step: i32) {
        if self.files.is_empty() {
            return;
        }
        let len = self.files.len() as i32;
        let current = self.selected_index.map(|i| i as i32).unwrap_or(-1);
        let next = (current + step).clamp(0, len - 1);
        self.select_index(next as usize);
    }

    /// Handles ArrowUp/ArrowDown for fast keyboard scrolling through the file
    /// list: an immediate step on first press, then a short delay before
    /// repeating very quickly while the key is held (geeqie-style browsing).
    fn handle_nav_keys(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let up_pressed = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down_pressed = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));

        let mut step = 0i32;
        if up_pressed {
            step = -1;
            self.nav_held_key = Some(egui::Key::ArrowUp);
            self.nav_next_repeat_at = now + NAV_INITIAL_DELAY;
        } else if down_pressed {
            step = 1;
            self.nav_held_key = Some(egui::Key::ArrowDown);
            self.nav_next_repeat_at = now + NAV_INITIAL_DELAY;
        } else if let Some(key) = self.nav_held_key {
            let still_held = ctx.input(|i| i.key_down(key));
            if !still_held {
                self.nav_held_key = None;
            } else if now >= self.nav_next_repeat_at {
                step = if key == egui::Key::ArrowUp { -1 } else { 1 };
                self.nav_next_repeat_at = now + NAV_REPEAT_INTERVAL;
            }
        }

        if step != 0 {
            self.step_selection(step);
        }
        if self.nav_held_key.is_some() {
            // Keep polling every frame while a key is held so the repeat
            // timer above fires promptly instead of waiting for the next
            // externally-triggered repaint.
            ctx.request_repaint();
        }
    }

}

/// Decodes an image to RGBA8. JPEGs go through libjpeg-turbo (the same
/// SIMD-accelerated decoder gdk-pixbuf/geeqie use), which is far faster than
/// the pure-Rust decoder in the `image` crate; everything else falls back to
/// `image::open`.
fn decode_to_rgba(path: &Path) -> Result<image::RgbaImage, String> {
    let is_jpeg = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "jpg" || e == "jpeg"
        })
        .unwrap_or(false);

    if is_jpeg {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        match turbojpeg::decompress_image::<image::Rgba<u8>>(&bytes) {
            Ok(rgba) => return Ok(rgba),
            Err(_) => { /* fall through to the generic decoder below */ }
        }
    }

    image::open(path)
        .map(|img| img.to_rgba8())
        .map_err(|e| e.to_string())
}

fn read_exif(path: &Path) -> Vec<(String, String)> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut bufreader = std::io::BufReader::new(file);
    let exif_reader = exif::Reader::new();
    let exif_data = match exif_reader.read_from_container(&mut bufreader) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for field in exif_data.fields() {
        let tag_name = field.tag.to_string();
        let value = field
            .display_value()
            .with_unit(&exif_data)
            .to_string();
        out.push((tag_name, value));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

impl eframe::App for GeeqApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_nav_keys(ctx);
        self.drain_load_messages(ctx);

        // Options panel: docked to the left edge, outside (left of) the nav
        // panel, so it slides open from the screen's left edge.
        egui::SidePanel::left("options_panel")
            .resizable(false)
            .default_width(220.0)
            .show_animated(ctx, self.show_options, |ui| {
                ui.heading("Options");
                ui.separator();
                if ui.checkbox(&mut self.show_hidden, "Show hidden folders/files").changed() {
                    self.rescan_dir();
                }
            });

        egui::SidePanel::left("nav_panel")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let toggle_label = if self.show_options { "⟨ Options" } else { "☰ Options" };
                    if ui.button(toggle_label).clicked() {
                        self.show_options = !self.show_options;
                    }
                    ui.heading("Folders");
                });

                ui.horizontal(|ui| {
                    if ui.button("Up").clicked() {
                        self.navigate_up();
                    }
                    if ui.button("Choose...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.navigate_to(dir);
                        }
                    }
                });

                let path_edit = ui.text_edit_singleline(&mut self.path_input);
                if path_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    let candidate = PathBuf::from(&self.path_input);
                    if candidate.is_dir() {
                        self.navigate_to(candidate);
                    } else {
                        self.error = Some("Not a valid directory".to_string());
                    }
                }

                ui.separator();

                // Folder surfer: subdirectories of the current directory.
                egui::ScrollArea::vertical()
                    .id_salt("subdirs_scroll")
                    .max_height(ui.available_height() * 0.35)
                    .show(ui, |ui| {
                        for dir in self.subdirs.clone() {
                            let name = dir
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            if ui.selectable_label(false, format!("📁 {name}")).double_clicked() {
                                self.navigate_to(dir);
                            }
                        }
                    });

                ui.separator();
                ui.label(format!("Images ({})", self.files.len()));

                // File list.
                egui::ScrollArea::vertical()
                    .id_salt("files_scroll")
                    .show(ui, |ui| {
                        for (idx, file) in self.files.clone().into_iter().enumerate() {
                            let name = file
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let is_selected = self.selected_index == Some(idx);
                            let resp = ui.selectable_label(is_selected, name);
                            if resp.clicked() {
                                self.select_index(idx);
                            }
                            if is_selected && self.nav_held_key.is_some() {
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, err);
                ui.separator();
            }

            let Some(loaded) = &self.loaded else {
                ui.centered_and_justified(|ui| {
                    ui.label("Select an image from the list on the left.");
                });
                return;
            };

            ui.horizontal(|ui| {
                let name = loaded.path.file_name().unwrap_or_default().to_string_lossy();
                if loaded.texture.is_some() {
                    ui.label(format!(
                        "{name}  ({}x{})",
                        loaded.size.x as i32,
                        loaded.size.y as i32
                    ));
                } else {
                    ui.label(format!("{name}  (decoding...)"));
                }
            });
            ui.separator();

            ui.columns(2, |columns| {
                // Left column: the image, scaled to fit, as soon as it's decoded.
                columns[0].centered_and_justified(|ui| {
                    match &loaded.texture {
                        Some(texture) => {
                            let avail = ui.available_size();
                            let scale = (avail.x / loaded.size.x)
                                .min(avail.y / loaded.size.y)
                                .min(f32::MAX);
                            let scale = if scale.is_finite() && scale > 0.0 {
                                scale
                            } else {
                                1.0
                            };
                            let draw_size = loaded.size * scale;
                            let sized_texture =
                                egui::load::SizedTexture::new(texture.id(), loaded.size);
                            ui.add(
                                egui::Image::from_texture(sized_texture)
                                    .fit_to_exact_size(draw_size),
                            );
                        }
                        None => {
                            ui.spinner();
                        }
                    }
                });

                // Right column: EXIF + histogram, populated independently as
                // each finishes computing in the background.
                let ui = &mut columns[1];
                ui.heading("Histogram");
                match &loaded.histogram {
                    Some(histogram) => draw_histogram(ui, histogram),
                    None => {
                        ui.label("Computing...");
                    }
                }

                ui.separator();
                ui.heading("EXIF");
                match &loaded.exif {
                    Some(exif) => {
                        egui::ScrollArea::vertical().id_salt("exif_scroll").show(ui, |ui| {
                            if exif.is_empty() {
                                ui.label("No EXIF data found.");
                            } else {
                                egui::Grid::new("exif_grid")
                                    .num_columns(2)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for (tag, value) in exif {
                                            ui.label(tag);
                                            ui.label(value);
                                            ui.end_row();
                                        }
                                    });
                            }
                        });
                    }
                    None => {
                        ui.label("Reading...");
                    }
                }
            });
        });
    }
}

fn draw_histogram(ui: &mut egui::Ui, hist: &Histogram) {
    let desired_size = egui::vec2(ui.available_width(), 120.0);
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 2.0, Color32::from_gray(20));

    let bin_width = rect.width() / 256.0;
    let channels: [(&[u32; 256], Color32); 3] = [
        (&hist.r, Color32::from_rgba_unmultiplied(255, 60, 60, 140)),
        (&hist.g, Color32::from_rgba_unmultiplied(60, 255, 60, 140)),
        (&hist.b, Color32::from_rgba_unmultiplied(80, 140, 255, 140)),
    ];

    for (values, color) in channels {
        for (i, &count) in values.iter().enumerate() {
            let frac = count as f32 / hist.max as f32;
            let bar_height = frac * rect.height();
            let x = rect.left() + i as f32 * bin_width;
            let top = egui::pos2(x, rect.bottom() - bar_height);
            let bottom = egui::pos2(x + bin_width.max(1.0), rect.bottom());
            painter.rect_filled(egui::Rect::from_min_max(top, bottom), 0.0, color);
        }
    }

    painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0, Color32::from_gray(80)));
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 750.0]),
        ..Default::default()
    };

    eframe::run_native(
        "geeq",
        options,
        Box::new(|_cc| Ok(Box::new(GeeqApp::new()))),
    )
}

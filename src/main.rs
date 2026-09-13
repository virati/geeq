// geeq: a minimal geeqie-2020-style image viewer.
// Left panel: folder navigator (top) + file list (bottom).
// Right panel: image view + EXIF metadata + RGB histogram.

use eframe::egui;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Where the per-directory activity weights are persisted between runs.
fn recent_dirs_file() -> Option<PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok()?;
    Some(config_home.join("geeq").join("recent_dirs.txt"))
}

fn load_dir_activity() -> HashMap<PathBuf, u32> {
    let mut map = HashMap::new();
    let Some(path) = recent_dirs_file() else { return map };
    let Ok(contents) = fs::read_to_string(path) else { return map };
    for line in contents.lines() {
        if let Some((weight, dir)) = line.split_once('\t') {
            if let Ok(weight) = weight.parse::<u32>() {
                map.insert(PathBuf::from(dir), weight);
            }
        }
    }
    map
}

/// Rewrites the whole file each time; activity is recorded per user action,
/// not per frame, so this stays cheap. Caps to the most active entries so
/// the file (and the eventual UI list) doesn't grow without bound.
fn save_dir_activity(map: &HashMap<PathBuf, u32>) {
    let Some(path) = recent_dirs_file() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut entries: Vec<(u32, &PathBuf)> = map.iter().map(|(p, &w)| (w, p)).collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(200);
    let content = entries
        .iter()
        .map(|(w, p)| format!("{w}\t{}", p.display()))
        .collect::<Vec<_>>()
        .join("\n");
    let _ = fs::write(path, content);
}

/// The classic exposure triangle for one photo, pulled straight from EXIF
/// without decoding the image. Used to plot how exposure settings were used
/// across a whole folder.
#[derive(Clone)]
struct ExposurePoint {
    path: PathBuf,
    fstop: f32,
    shutter_secs: f32,
    iso: f32,
}

fn exif_rational_f32(value: &exif::Value) -> Option<f32> {
    match value {
        exif::Value::Rational(v) => v.first().map(exif::Rational::to_f32),
        exif::Value::SRational(v) => v.first().map(exif::SRational::to_f32),
        _ => None,
    }
}

fn exif_uint_f32(value: &exif::Value) -> Option<f32> {
    match value {
        exif::Value::Short(v) => v.first().map(|&x| x as f32),
        exif::Value::Long(v) => v.first().map(|&x| x as f32),
        _ => None,
    }
}

/// Reads just the three exposure-triangle fields from a file's EXIF header
/// (no image decode), skipping the photo if any of them is missing.
fn read_exposure_point(path: &Path) -> Option<ExposurePoint> {
    let file = fs::File::open(path).ok()?;
    let mut bufreader = std::io::BufReader::new(file);
    let exif_data = exif::Reader::new().read_from_container(&mut bufreader).ok()?;

    let fstop = exif_data
        .get_field(exif::Tag::FNumber, exif::In::PRIMARY)
        .and_then(|f| exif_rational_f32(&f.value))?;
    let shutter_secs = exif_data
        .get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
        .and_then(|f| exif_rational_f32(&f.value))?;
    let iso = exif_data
        .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
        .and_then(|f| exif_uint_f32(&f.value))?;

    Some(ExposurePoint { path: path.to_path_buf(), fstop, shutter_secs, iso })
}

/// A focus-measure score for one photo (variance of the Laplacian), used to
/// build a sharpness histogram across a folder.
struct SharpnessPoint {
    path: PathBuf,
    score: f32,
}

/// Variance of the Laplacian: a standard, cheap full-image focus measure.
/// A sharp photo has lots of strong edges, so the second-derivative
/// response has high variance; a blurry one is smooth and stays near zero
/// everywhere. Computed on a small grayscale thumbnail rather than the full
/// decode - focus/blur is a low-frequency property of the image, so this
/// stays a good proxy while being orders of magnitude cheaper.
fn laplacian_variance(gray: &image::GrayImage) -> f32 {
    let (w, h) = (gray.width() as i64, gray.height() as i64);
    if w < 3 || h < 3 {
        return 0.0;
    }
    let get = |x: i64, y: i64| gray.get_pixel(x as u32, y as u32)[0] as f32;

    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut n = 0u64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let lap = -4.0 * get(x, y) + get(x - 1, y) + get(x + 1, y) + get(x, y - 1) + get(x, y + 1);
            sum += lap as f64;
            sum_sq += (lap as f64) * (lap as f64);
            n += 1;
        }
    }
    if n == 0 {
        return 0.0;
    }
    let mean = sum / n as f64;
    ((sum_sq / n as f64) - mean * mean).max(0.0) as f32
}

fn compute_sharpness(path: &Path) -> Option<f32> {
    let rgba = decode_to_rgba(path).ok()?;
    let dyn_img = image::DynamicImage::ImageRgba8(rgba);
    let thumb = dyn_img.thumbnail(256, 256).to_luma8();
    Some(laplacian_variance(&thumb))
}

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

/// Raw numeric shooting settings for the current photo, used to draw the
/// small radial overlay on top of the image. `None` fields (missing EXIF)
/// just drop that spoke from the plot.
#[derive(Clone, Copy, Default)]
struct RadialStats {
    focal_mm: Option<f32>,
    shutter_secs: Option<f32>,
    fstop: Option<f32>,
    iso: Option<f32>,
}

/// EXIF fields split into the handful most people actually look at (ISO,
/// shutter speed, f-stop, date) and everything else, so the UI can show the
/// key info up front and tuck the rest behind a collapsible section.
struct ExifInfo {
    key: Vec<(String, String)>,
    other: Vec<(String, String)>,
    radial: RadialStats,
}

/// An image load is split into independent pieces that each arrive on their
/// own schedule: the texture appears as soon as it's decoded rather than
/// waiting on the histogram or EXIF, which are computed in parallel on
/// background threads so the UI thread is never blocked by decoding.
struct LoadedImage {
    path: PathBuf,
    texture: Option<TextureHandle>,
    size: egui::Vec2,
    exif: Option<ExifInfo>,
    histogram: Option<Histogram>,
}

enum LoadMsg {
    Texture(u64, PathBuf, ColorImage),
    Histogram(u64, PathBuf, Histogram),
    Exif(u64, PathBuf, ExifInfo),
    Error(u64, String),
}

const NAV_INITIAL_DELAY: f64 = 0.25;
const NAV_REPEAT_INTERVAL: f64 = 0.03;
const ZOOM_MIN: f32 = 1.0;
const ZOOM_MAX: f32 = 8.0;
const ZOOM_KEY_STEP: f32 = 1.25;

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
    show_info_panel: bool,
    zoom: f32,
    /// Display-only rotation, in quarter turns clockwise (0..=3). Does not
    /// touch the file on disk.
    rotation: u8,
    clip_stack: Vec<PathBuf>,
    generation: u64,
    load_tx: mpsc::Sender<LoadMsg>,
    load_rx: mpsc::Receiver<LoadMsg>,
    /// Exposure-triangle points for every image in the current folder,
    /// scanned from EXIF headers in the background (no decoding).
    exposure_points: Vec<ExposurePoint>,
    /// Sharpness (Laplacian variance) for every image in the current folder,
    /// scanned from thumbnail decodes in the background.
    sharpness_points: Vec<SharpnessPoint>,
    dir_generation: u64,
    exposure_tx: mpsc::Sender<(u64, Vec<ExposurePoint>)>,
    exposure_rx: mpsc::Receiver<(u64, Vec<ExposurePoint>)>,
    sharpness_tx: mpsc::Sender<(u64, Vec<SharpnessPoint>)>,
    sharpness_rx: mpsc::Receiver<(u64, Vec<SharpnessPoint>)>,
    /// How many operations (viewing/rotating/clipping a file) have happened
    /// in each directory, persisted across runs so "recent" is meaningful.
    dir_activity: HashMap<PathBuf, u32>,
    /// Small thumbnails for the clip-stack popup, decoded in the background
    /// and cached so re-adding/toggling a file doesn't re-decode it.
    thumbnail_cache: HashMap<PathBuf, TextureHandle>,
    thumbnail_pending: HashSet<PathBuf>,
    thumb_tx: mpsc::Sender<(PathBuf, ColorImage)>,
    thumb_rx: mpsc::Receiver<(PathBuf, ColorImage)>,
}

impl GeeqApp {
    fn new() -> Self {
        let start_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let (load_tx, load_rx) = mpsc::channel();
        let (exposure_tx, exposure_rx) = mpsc::channel();
        let (sharpness_tx, sharpness_rx) = mpsc::channel();
        let (thumb_tx, thumb_rx) = mpsc::channel();

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
            show_info_panel: true,
            zoom: 1.0,
            rotation: 0,
            clip_stack: Vec::new(),
            generation: 0,
            load_tx,
            load_rx,
            exposure_points: Vec::new(),
            sharpness_points: Vec::new(),
            dir_generation: 0,
            exposure_tx,
            exposure_rx,
            sharpness_tx,
            sharpness_rx,
            dir_activity: load_dir_activity(),
            thumbnail_cache: HashMap::new(),
            thumbnail_pending: HashSet::new(),
            thumb_tx,
            thumb_rx,
        };
        app.refresh_dir();
        app
    }

    /// Records one operation against the current directory and persists the
    /// updated weights immediately (cheap: only called on user actions, and
    /// the file is small).
    fn record_activity(&mut self) {
        *self.dir_activity.entry(self.current_dir.clone()).or_insert(0) += 1;
        save_dir_activity(&self.dir_activity);
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
        self.spawn_dir_scans();
    }

    /// Scans every image in the current folder in the background: exposure
    /// EXIF fields (header-only, cheap) and sharpness (needs a thumbnail
    /// decode, heavier). Both runs share one generation number so navigating
    /// away cancels (ignores the results of) both consistently.
    fn spawn_dir_scans(&mut self) {
        self.dir_generation += 1;
        self.exposure_points.clear();
        self.sharpness_points.clear();
        let gen = self.dir_generation;

        let exposure_files = self.files.clone();
        let exposure_tx = self.exposure_tx.clone();
        thread::spawn(move || {
            let points: Vec<ExposurePoint> =
                exposure_files.iter().filter_map(|p| read_exposure_point(p)).collect();
            let _ = exposure_tx.send((gen, points));
        });

        let sharpness_files = self.files.clone();
        let sharpness_tx = self.sharpness_tx.clone();
        thread::spawn(move || {
            let points: Vec<SharpnessPoint> = sharpness_files
                .iter()
                .filter_map(|p| {
                    let score = compute_sharpness(p)?;
                    Some(SharpnessPoint { path: p.clone(), score })
                })
                .collect();
            let _ = sharpness_tx.send((gen, points));
        });
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
            self.record_activity();
        }
    }

    /// Right-click adds a file's path to a running stack and refreshes the
    /// system clipboard with the whole stack, shell-quoted and
    /// space-separated, so it can be pasted straight into a terminal as
    /// arguments to `cp`/`mv`/etc.
    fn add_to_clip_stack(&mut self, ctx: &egui::Context, path: PathBuf) {
        if !self.clip_stack.contains(&path) {
            self.ensure_thumbnail(path.clone());
            self.clip_stack.push(path);
        }
        ctx.copy_text(self.clip_stack_text());
        self.record_activity();
    }

    /// `\` toggles the currently open photo in/out of the clip stack -
    /// "included"/"excluded" from whatever batch you're building up.
    fn toggle_clip_stack(&mut self, ctx: &egui::Context, path: PathBuf) {
        if let Some(pos) = self.clip_stack.iter().position(|p| *p == path) {
            self.clip_stack.remove(pos);
        } else {
            self.ensure_thumbnail(path.clone());
            self.clip_stack.push(path);
        }
        ctx.copy_text(self.clip_stack_text());
        self.record_activity();
    }

    /// Kicks off a background thumbnail decode for the clip-stack popup,
    /// unless one's already cached or in flight for this path.
    fn ensure_thumbnail(&mut self, path: PathBuf) {
        if self.thumbnail_cache.contains_key(&path) || self.thumbnail_pending.contains(&path) {
            return;
        }
        self.thumbnail_pending.insert(path.clone());
        let tx = self.thumb_tx.clone();
        thread::spawn(move || {
            if let Ok(rgba) = decode_to_rgba(&path) {
                let thumb = image::DynamicImage::ImageRgba8(rgba).thumbnail(64, 64).to_rgba8();
                let (w, h) = (thumb.width() as usize, thumb.height() as usize);
                let color_image = ColorImage::from_rgba_unmultiplied([w, h], thumb.as_raw());
                let _ = tx.send((path, color_image));
            }
        });
    }

    fn clip_stack_text(&self) -> String {
        self.clip_stack
            .iter()
            .map(|p| shell_quote(p))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn clear_clip_stack(&mut self, ctx: &egui::Context) {
        self.clip_stack.clear();
        ctx.copy_text(String::new());
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
        self.zoom = 1.0;
        self.rotation = 0;
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

        while let Ok((gen, points)) = self.exposure_rx.try_recv() {
            if gen == self.dir_generation {
                self.exposure_points = points;
            }
        }
        while let Ok((gen, points)) = self.sharpness_rx.try_recv() {
            if gen == self.dir_generation {
                self.sharpness_points = points;
            }
        }

        while let Ok((path, color_image)) = self.thumb_rx.try_recv() {
            let texture = ctx.load_texture(
                format!("thumb:{}", path.display()),
                color_image,
                TextureOptions::LINEAR,
            );
            self.thumbnail_cache.insert(path.clone(), texture);
            self.thumbnail_pending.remove(&path);
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
        // ArrowUp/ArrowDown and `[`/`]` are equivalent - the brackets sit
        // right next to the other hotkeys clustered at the top-right of a
        // keyboard, so they're a closer reach than the arrow cluster.
        const UP_KEYS: [egui::Key; 2] = [egui::Key::ArrowUp, egui::Key::OpenBracket];
        const DOWN_KEYS: [egui::Key; 2] = [egui::Key::ArrowDown, egui::Key::CloseBracket];

        let now = ctx.input(|i| i.time);
        let up_pressed = ctx.input(|i| UP_KEYS.iter().any(|&k| i.key_pressed(k)));
        let down_pressed = ctx.input(|i| DOWN_KEYS.iter().any(|&k| i.key_pressed(k)));

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
            let keys = if key == egui::Key::ArrowUp { &UP_KEYS } else { &DOWN_KEYS };
            let still_held = ctx.input(|i| keys.iter().any(|&k| i.key_down(k)));
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

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

const EXIF_KEY_TAGS: &[(exif::Tag, &str)] = &[
    (exif::Tag::DateTimeOriginal, "Date Taken"),
    (exif::Tag::PhotographicSensitivity, "ISO"),
    (exif::Tag::ExposureTime, "Shutter Speed"),
    (exif::Tag::FNumber, "F-Stop"),
    (exif::Tag::FocalLength, "Focal Length"),
];

fn read_exif(path: &Path) -> ExifInfo {
    let empty = || ExifInfo { key: Vec::new(), other: Vec::new(), radial: RadialStats::default() };
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return empty(),
    };
    let mut bufreader = std::io::BufReader::new(file);
    let exif_reader = exif::Reader::new();
    let exif_data = match exif_reader.read_from_container(&mut bufreader) {
        Ok(d) => d,
        Err(_) => return empty(),
    };

    let mut key = Vec::new();
    let mut other = Vec::new();
    for field in exif_data.fields() {
        let value = field.display_value().with_unit(&exif_data).to_string();
        match EXIF_KEY_TAGS.iter().find(|(tag, _)| *tag == field.tag) {
            Some((_, label)) => key.push((label.to_string(), value)),
            None => other.push((field.tag.to_string(), value)),
        }
    }

    // Plain "Date" (0x132) is a fallback for cameras that don't write
    // DateTimeOriginal.
    if !key.iter().any(|(label, _)| *label == "Date Taken") {
        if let Some(field) = exif_data.get_field(exif::Tag::DateTime, exif::In::PRIMARY) {
            let value = field.display_value().with_unit(&exif_data).to_string();
            key.push(("Date Taken".to_string(), value));
        }
    }

    let preferred_order = ["Date Taken", "ISO", "Shutter Speed", "F-Stop", "Focal Length"];
    key.sort_by_key(|(label, _)| {
        preferred_order.iter().position(|o| o == label).unwrap_or(usize::MAX)
    });
    other.sort_by(|a, b| a.0.cmp(&b.0));

    let radial = RadialStats {
        focal_mm: exif_data
            .get_field(exif::Tag::FocalLength, exif::In::PRIMARY)
            .and_then(|f| exif_rational_f32(&f.value)),
        shutter_secs: exif_data
            .get_field(exif::Tag::ExposureTime, exif::In::PRIMARY)
            .and_then(|f| exif_rational_f32(&f.value)),
        fstop: exif_data
            .get_field(exif::Tag::FNumber, exif::In::PRIMARY)
            .and_then(|f| exif_rational_f32(&f.value)),
        iso: exif_data
            .get_field(exif::Tag::PhotographicSensitivity, exif::In::PRIMARY)
            .and_then(|f| exif_uint_f32(&f.value)),
    };

    ExifInfo { key, other, radial }
}

impl eframe::App for GeeqApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_nav_keys(ctx);
        self.drain_load_messages(ctx);

        if ctx.input(|i| i.key_pressed(egui::Key::Backslash)) {
            if let Some(path) = self.selected_file.clone() {
                self.toggle_clip_stack(ctx, path);
            }
        }

        // A transparent popup of thumbnails for whatever's in the clip
        // stack, floating over the left panel.
        if !self.clip_stack.is_empty() {
            egui::Window::new("clip_stack_popup")
                .title_bar(false)
                .resizable(false)
                .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
                .frame(egui::Frame::popup(&ctx.style()).fill(Color32::from_black_alpha(210)))
                .show(ctx, |ui| {
                    ui.label(format!("📋 Clip stack ({})", self.clip_stack.len()));
                    egui::ScrollArea::horizontal().max_width(500.0).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for path in &self.clip_stack {
                                ui.vertical(|ui| {
                                    ui.set_width(70.0);
                                    match self.thumbnail_cache.get(path) {
                                        Some(texture) => {
                                            let sized = egui::load::SizedTexture::new(
                                                texture.id(),
                                                egui::vec2(64.0, 64.0),
                                            );
                                            ui.add(egui::Image::from_texture(sized));
                                        }
                                        None => {
                                            ui.add_sized([64.0, 64.0], egui::Spinner::new());
                                        }
                                    }
                                    let name = path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    ui.add(egui::Label::new(egui::RichText::new(name).small()).truncate());
                                });
                            }
                        });
                    });
                });
        }

        // Directory-wide displays (phase plot, sharpness histogram) span the
        // full window width, so this is registered before any SidePanel
        // claims a vertical strip - it becomes the outermost bottom panel,
        // with the nav panel and image view rendered in the space above it.
        egui::TopBottomPanel::bottom("dir_panel")
            .resizable(false)
            .min_height(180.0)
            .show(ctx, |ui| {
                let current_path = self.loaded.as_ref().map(|l| l.path.as_path());
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.label(format!(
                            "Exposure phase plot — {} photo(s)",
                            self.exposure_points.len()
                        ));
                        draw_phase_plot_3d(ui, &self.exposure_points, current_path);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.label(format!(
                            "Sharpness — {} photo(s)",
                            self.sharpness_points.len()
                        ));
                        draw_sharpness_histogram(ui, &self.sharpness_points, current_path);
                    });
                });
            });

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
                    self.spawn_dir_scans();
                }

                ui.separator();
                ui.heading("Recent Directories");
                ui.label("Weighted by files viewed/rotated/clipped there.");
                if self.dir_activity.is_empty() {
                    ui.label("No activity recorded yet.");
                } else {
                    let mut recent: Vec<(PathBuf, u32)> =
                        self.dir_activity.iter().map(|(p, &w)| (p.clone(), w)).collect();
                    recent.sort_by(|a, b| b.1.cmp(&a.1));
                    recent.truncate(15);

                    let mut jump_to = None;
                    egui::ScrollArea::vertical()
                        .id_salt("recent_dirs_scroll")
                        .max_height(240.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (dir, weight) in &recent {
                                let is_current = *dir == self.current_dir;
                                let label = format!("{}  ({weight})", dir.display());
                                if ui.selectable_label(is_current, label).clicked() {
                                    jump_to = Some(dir.clone());
                                }
                            }
                        });
                    if let Some(dir) = jump_to {
                        self.navigate_to(dir);
                    }
                }
            });

        egui::SidePanel::left("nav_panel")
            .resizable(true)
            .default_width(280.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin { left: 8, right: 0, top: 8, bottom: 8 }),
            )
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
                    .auto_shrink([false, false])
                    .max_height(ui.available_height() * 0.35)
                    .show(ui, |ui| {
                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::LEFT),
                            |ui| {
                                if ui.selectable_label(false, "📁 .").double_clicked() {
                                    self.rescan_dir();
                                }
                                if let Some(parent) = self.current_dir.parent() {
                                    if ui.selectable_label(false, "📁 ..").double_clicked() {
                                        self.navigate_to(parent.to_path_buf());
                                    }
                                }
                                for dir in self.subdirs.clone() {
                                    let name = dir
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    if ui
                                        .selectable_label(false, format!("📁 {name}"))
                                        .double_clicked()
                                    {
                                        self.navigate_to(dir);
                                    }
                                }
                            },
                        );
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(format!("Images ({})", self.files.len()));
                    if !self.clip_stack.is_empty() {
                        ui.label(format!("📋 {}", self.clip_stack.len()));
                        if ui
                            .small_button("Clear")
                            .on_hover_text("Clear the clipboard stack")
                            .clicked()
                        {
                            self.clear_clip_stack(ctx);
                        }
                    }
                });

                // File list. Right-click a row to add its path to the
                // clipboard stack (paste into a terminal for cp/mv/etc).
                egui::ScrollArea::vertical()
                    .id_salt("files_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.with_layout(
                            egui::Layout::top_down_justified(egui::Align::LEFT),
                            |ui| {
                                for (idx, file) in self.files.clone().into_iter().enumerate() {
                                    let name = file
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let label = if self.clip_stack.contains(&file) {
                                        format!("📋 {name}")
                                    } else {
                                        name
                                    };
                                    let is_selected = self.selected_index == Some(idx);
                                    let resp = ui
                                        .selectable_label(is_selected, label)
                                        .on_hover_text("Right-click to copy path to clipboard stack");
                                    if resp.clicked() {
                                        self.select_index(idx);
                                    }
                                    if resp.secondary_clicked() {
                                        self.add_to_clip_stack(ctx, file);
                                    }
                                    if is_selected && self.nav_held_key.is_some() {
                                        resp.scroll_to_me(Some(egui::Align::Center));
                                    }
                                }
                            },
                        );
                    });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(Color32::LIGHT_RED, err);
                ui.separator();
            }

            if self.loaded.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Select an image from the list on the left.");
                });
                return;
            }

            let mut toggle_info_panel = false;
            {
                let loaded = self.loaded.as_ref().unwrap();
                ui.horizontal(|ui| {
                    let name = loaded.path.file_name().unwrap_or_default().to_string_lossy();
                    if loaded.texture.is_some() {
                        ui.label(format!(
                            "{name}  ({}x{})  zoom {:.0}%",
                            loaded.size.x as i32,
                            loaded.size.y as i32,
                            self.zoom * 100.0
                        ));
                    } else {
                        ui.label(format!("{name}  (decoding...)"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = if self.show_info_panel { "Info ▶" } else { "◀ Info" };
                        if ui
                            .button(label)
                            .on_hover_text("Collapse/expand the histogram/EXIF panel")
                            .clicked()
                        {
                            toggle_info_panel = true;
                        }
                    });
                });
            }
            if toggle_info_panel {
                self.show_info_panel = !self.show_info_panel;
            }
            ui.separator();

            // Histogram + EXIF live in a resizable panel docked to the right
            // of the remaining central space, so the divider can be dragged.
            // It can be collapsed away entirely (sliding out to the right)
            // to give the image more room.
            egui::SidePanel::right("info_panel")
                .resizable(true)
                .default_width(340.0)
                .width_range(220.0..=700.0)
                .show_animated_inside(ui, self.show_info_panel, |ui| {
                    let loaded = self.loaded.as_ref().unwrap();
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
                            egui::ScrollArea::vertical()
                                .id_salt("exif_scroll")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if exif.key.is_empty() && exif.other.is_empty() {
                                        ui.label("No EXIF data found.");
                                        return;
                                    }
                                    if !exif.key.is_empty() {
                                        egui::Grid::new("exif_key_grid")
                                            .num_columns(2)
                                            .striped(true)
                                            .show(ui, |ui| {
                                                for (tag, value) in &exif.key {
                                                    ui.label(tag);
                                                    ui.label(value);
                                                    ui.end_row();
                                                }
                                            });
                                    }
                                    if !exif.other.is_empty() {
                                        ui.add_space(4.0);
                                        egui::CollapsingHeader::new(format!(
                                            "Other EXIF ({})",
                                            exif.other.len()
                                        ))
                                        .default_open(false)
                                        .show(ui, |ui| {
                                            egui::Grid::new("exif_other_grid")
                                                .num_columns(2)
                                                .striped(true)
                                                .show(ui, |ui| {
                                                    for (tag, value) in &exif.other {
                                                        ui.label(tag);
                                                        ui.label(value);
                                                        ui.end_row();
                                                    }
                                                });
                                        });
                                    }
                                });
                        }
                        None => {
                            ui.label("Reading...");
                        }
                    }
                });

            // A rotate bar docked to the bottom of the (remaining) image pane.
            egui::TopBottomPanel::bottom("rotate_bar")
                .resizable(false)
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("⟲").on_hover_text("Rotate left (display only)").clicked() {
                            self.rotation = (self.rotation + 3) % 4;
                            self.record_activity();
                        }
                        if ui.button("⟳").on_hover_text("Rotate right (display only)").clicked() {
                            self.rotation = (self.rotation + 1) % 4;
                            self.record_activity();
                        }
                        ui.label(format!("{}°", self.rotation as u32 * 90));
                        if self.rotation != 0 && ui.button("Reset").clicked() {
                            self.rotation = 0;
                        }
                    });
                });

            // Whatever space is left is the image viewer.
            let image_rect = ui.available_rect_before_wrap();
            let avail = image_rect.size();
            let loaded = self.loaded.as_ref().unwrap();

            match &loaded.texture {
                Some(texture) => {
                    // Fit-to-window scale, preserving aspect ratio, then
                    // magnified by the zoom factor. Zooming in just enlarges
                    // the already-decoded texture with bilinear filtering
                    // (cheap, low-frequency/blurry magnification, no re-decode),
                    // so it stays instant even on large images.
                    //
                    // A 90/270 rotation swaps which axis is width vs height
                    // on screen, so the fit computation uses the *rotated*
                    // footprint, then the pre-rotation widget rect is
                    // allocated with axes swapped back so that egui's mesh
                    // rotation (which spins the rect about its own center)
                    // lands exactly on that footprint.
                    let rotated_odd = self.rotation % 2 == 1;
                    let (fit_w, fit_h) = if rotated_odd {
                        (loaded.size.y, loaded.size.x)
                    } else {
                        (loaded.size.x, loaded.size.y)
                    };
                    let fit_scale = (avail.x / fit_w).min(avail.y / fit_h);
                    let fit_scale = if fit_scale.is_finite() && fit_scale > 0.0 {
                        fit_scale
                    } else {
                        1.0
                    };

                    // '+'/'-' zoom regardless of which pane has focus; the
                    // scroll wheel only zooms while hovering the image (so it
                    // doesn't fight scrolling the file list or EXIF panel).
                    // Either way, zooming keeps whatever content point is
                    // currently at the viewport's center still centered
                    // afterwards, by adjusting the scroll offset to compensate
                    // rather than always growing from a corner.
                    let hovering = ui.rect_contains_pointer(image_rect);
                    let scroll = if hovering { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
                    let zoom_in = ui.input(|i| {
                        i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals)
                    });
                    let zoom_out = ui.input(|i| i.key_pressed(egui::Key::Minus));
                    let zoom_reset = hovering && ui.input(|i| i.key_pressed(egui::Key::Num0));

                    let mut new_zoom = self.zoom;
                    if scroll != 0.0 {
                        new_zoom *= (1.0 + scroll * 0.002).clamp(0.5, 2.0);
                    }
                    if zoom_in {
                        new_zoom *= ZOOM_KEY_STEP;
                    }
                    if zoom_out {
                        new_zoom /= ZOOM_KEY_STEP;
                    }
                    if zoom_reset {
                        new_zoom = ZOOM_MIN;
                    }
                    new_zoom = new_zoom.clamp(ZOOM_MIN, ZOOM_MAX);

                    if (new_zoom - self.zoom).abs() > f32::EPSILON {
                        let scroll_id = ui.make_persistent_id("image_scroll");
                        let old_draw_size = egui::vec2(fit_w, fit_h) * fit_scale * self.zoom;
                        let old_offset = egui::containers::scroll_area::State::load(ui.ctx(), scroll_id)
                            .map(|s| s.offset)
                            .unwrap_or(egui::Vec2::ZERO);
                        let fraction = zoom_center_fraction(old_offset, avail, old_draw_size);

                        self.zoom = new_zoom;

                        let new_draw_size = egui::vec2(fit_w, fit_h) * fit_scale * new_zoom;
                        let new_offset = zoom_offset_for_fraction(fraction, avail, new_draw_size);
                        let mut state =
                            egui::containers::scroll_area::State::load(ui.ctx(), scroll_id)
                                .unwrap_or_default();
                        state.offset = new_offset;
                        state.store(ui.ctx(), scroll_id);
                    }

                    let scale = fit_scale * self.zoom;
                    let screen_size = egui::vec2(fit_w * scale, fit_h * scale);
                    let alloc_size = if rotated_odd {
                        egui::vec2(screen_size.y, screen_size.x)
                    } else {
                        screen_size
                    };
                    let sized_texture = egui::load::SizedTexture::new(texture.id(), loaded.size);

                    let add_image = |ui: &mut egui::Ui| {
                        let mut image = egui::Image::from_texture(sized_texture)
                            .fit_to_exact_size(alloc_size);
                        if self.rotation != 0 {
                            let angle = self.rotation as f32 * std::f32::consts::FRAC_PI_2;
                            image = image.rotate(angle, egui::vec2(0.5, 0.5));
                        }
                        ui.add(image);
                    };

                    // Always go through the same ScrollArea, even when the
                    // image comfortably fits (no scrollbars appear then) -
                    // keeping one rendering path means offset (0,0) always
                    // means "centered", which is what the zoom-centering
                    // math above assumes.
                    egui::ScrollArea::both()
                        .id_salt("image_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.set_min_size(avail);
                            ui.centered_and_justified(add_image);
                        });

                    if let Some(exif) = &loaded.exif {
                        draw_radial_overlay(ui, image_rect.min, &exif.radial);
                    }
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                    });
                }
            }
        });
    }
}

/// Where, as a 0..1 fraction of the image itself, the viewport's center
/// currently is. Used to keep that same point centered when zoom changes.
///
/// The image sits centered inside a `max(draw_size, avail)`-sized region
/// (see `zoom_offset_for_fraction` / the `set_min_size` in the render code),
/// so on an axis where the image is smaller than the viewport there's
/// padding to account for, not just a raw scroll offset - otherwise the
/// fraction comes out wrong on that axis and zooming visibly drifts toward
/// a corner instead of staying centered.
fn zoom_center_fraction(offset: egui::Vec2, avail: egui::Vec2, draw_size: egui::Vec2) -> egui::Vec2 {
    let content_size = egui::vec2(draw_size.x.max(avail.x), draw_size.y.max(avail.y));
    let pad = (content_size - draw_size) * 0.5;
    let raw = offset + avail * 0.5 - pad;
    egui::vec2(
        if draw_size.x > 0.0 { raw.x / draw_size.x } else { 0.5 },
        if draw_size.y > 0.0 { raw.y / draw_size.y } else { 0.5 },
    )
}

/// Inverse of `zoom_center_fraction`: the scroll offset that puts the given
/// fraction of the image back at the viewport's center.
fn zoom_offset_for_fraction(fraction: egui::Vec2, avail: egui::Vec2, draw_size: egui::Vec2) -> egui::Vec2 {
    let content_size = egui::vec2(draw_size.x.max(avail.x), draw_size.y.max(avail.y));
    let pad = (content_size - draw_size) * 0.5;
    let raw = fraction * draw_size + pad - avail * 0.5;
    egui::vec2(raw.x.max(0.0), raw.y.max(0.0))
}

/// Log-scale normalize: camera settings (shutter speed, ISO, focal length,
/// even f-stop) are perceived and used in roughly geometric steps, so a
/// linear 0..1 map would bunch everything near one end.
fn normalize_log(value: f32, min: f32, max: f32) -> f32 {
    let value = value.clamp(min, max);
    (value.log2() - min.log2()) / (max.log2() - min.log2())
}

/// A small radar-style overlay pinned to the top-left of the image pane,
/// showing the current shot's focal length, shutter speed, f-stop, and ISO
/// as four normalized spokes. Purely a HUD readout, drawn on top of
/// whatever's already rendered rather than reserving its own layout space.
fn draw_radial_overlay(ui: &egui::Ui, anchor: egui::Pos2, stats: &RadialStats) {
    if stats.focal_mm.is_none()
        && stats.shutter_secs.is_none()
        && stats.fstop.is_none()
        && stats.iso.is_none()
    {
        return;
    }

    let radius = 34.0;
    let margin = 14.0;
    let center = anchor + egui::vec2(radius + margin, radius + margin);
    let painter = ui.painter();

    painter.circle_filled(center, radius + 16.0, Color32::from_black_alpha(150));

    let axes: [(&str, Option<f32>, f32, f32); 4] = [
        ("Focal", stats.focal_mm, 10.0, 400.0),
        ("Shutter", stats.shutter_secs, 1.0 / 8000.0, 30.0),
        ("F-Stop", stats.fstop, 1.0, 22.0),
        ("ISO", stats.iso, 100.0, 12800.0),
    ];

    let mut poly = Vec::with_capacity(4);
    for (i, (label, value, min, max)) in axes.iter().enumerate() {
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::FRAC_PI_2;
        let dir = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment(
            [center, center + dir * radius],
            egui::Stroke::new(1.0, Color32::from_gray(110)),
        );
        painter.text(
            center + dir * (radius + 9.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(9.5),
            Color32::LIGHT_GRAY,
        );
        let norm = value.map(|v| normalize_log(v, *min, *max)).unwrap_or(0.0);
        poly.push(center + dir * (radius * norm));
    }

    painter.add(egui::Shape::convex_polygon(
        poly,
        Color32::from_rgba_unmultiplied(90, 170, 255, 90),
        egui::Stroke::new(1.5, Color32::from_rgb(120, 190, 255)),
    ));
}

/// A distinct color per point (evenly spaced hue), with a caller-chosen
/// alpha - used to make each photo's dot in the phase plot distinguishable
/// while dimming everything except whichever one is currently open.
fn point_color(index: usize, total: usize, alpha: u8) -> Color32 {
    let hue = if total > 0 { (index as f32 / total as f32).fract() } else { 0.0 };
    let rgb: Color32 = egui::ecolor::Hsva::new(hue, 0.75, 0.95, 1.0).into();
    Color32::from_rgba_unmultiplied(rgb.r(), rgb.g(), rgb.b(), alpha)
}

/// A ternary/radar-style projection of the exposure triangle (f-stop, ISO,
/// shutter speed) onto one 2D plot: three axes spaced 120 degrees apart,
/// each point placed at the vector sum of its three (log-normalized)
/// coordinates along those axes. Not a literal 3D scatter (egui_plot only
/// does 2D, and a real rotatable 3D view is a lot of machinery for a photo
/// viewer), but it shows all three variables in one picture rather than
/// three separate pairwise plots.
fn draw_phase_plot_3d(ui: &mut egui::Ui, points: &[ExposurePoint], current: Option<&Path>) {
    let size = 260.0;
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(20));

    if points.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No exposure data yet",
            egui::FontId::proportional(12.0),
            Color32::GRAY,
        );
        return;
    }

    let center = rect.center();
    let radius = size * 0.36;

    let axes: [(&str, f32, f32); 3] = [
        ("F-Stop", 1.0, 22.0),
        ("ISO", 100.0, 12800.0),
        ("Shutter", 1.0 / 8000.0, 30.0),
    ];
    let dirs: [egui::Vec2; 3] = std::array::from_fn(|i| {
        let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * (std::f32::consts::TAU / 3.0);
        egui::vec2(angle.cos(), angle.sin())
    });

    for (i, (label, _, _)) in axes.iter().enumerate() {
        painter.line_segment(
            [center, center + dirs[i] * radius],
            egui::Stroke::new(1.0, Color32::from_gray(90)),
        );
        painter.text(
            center + dirs[i] * (radius + 14.0),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(11.0),
            Color32::LIGHT_GRAY,
        );
    }

    let n = points.len();
    for (i, p) in points.iter().enumerate() {
        let is_current = current == Some(p.path.as_path());
        let nf = normalize_log(p.fstop, axes[0].1, axes[0].2);
        let ni = normalize_log(p.iso, axes[1].1, axes[1].2);
        let ns = normalize_log(p.shutter_secs, axes[2].1, axes[2].2);
        let pos = center + dirs[0] * (nf * radius) + dirs[1] * (ni * radius) + dirs[2] * (ns * radius);

        let alpha = if is_current { 255 } else { 51 };
        let color = point_color(i, n, alpha);
        let point_radius = if is_current { 5.0 } else { 3.0 };
        painter.circle_filled(pos, point_radius, color);
        if is_current {
            painter.circle_stroke(pos, point_radius + 1.5, egui::Stroke::new(1.5, Color32::WHITE));
        }
    }
}

/// A histogram of sharpness (Laplacian variance) across every image in the
/// folder, with a marker for wherever the currently open photo falls.
fn draw_sharpness_histogram(ui: &mut egui::Ui, points: &[SharpnessPoint], current: Option<&Path>) {
    let desired_size = egui::vec2(320.0, 140.0);
    let (rect, _response) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, Color32::from_gray(20));

    if points.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No sharpness data yet",
            egui::FontId::proportional(12.0),
            Color32::GRAY,
        );
        return;
    }

    let min = points.iter().map(|p| p.score).fold(f32::INFINITY, f32::min);
    let max = points.iter().map(|p| p.score).fold(f32::NEG_INFINITY, f32::max);
    let range = (max - min).max(1e-6);

    const BINS: usize = 40;
    let mut counts = [0u32; BINS];
    for p in points {
        let bin = (((p.score - min) / range) * (BINS as f32 - 1.0)).round() as usize;
        counts[bin.min(BINS - 1)] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(1).max(1);

    let bin_width = rect.width() / BINS as f32;
    for (i, &count) in counts.iter().enumerate() {
        let frac = count as f32 / max_count as f32;
        let bar_height = frac * rect.height();
        let x = rect.left() + i as f32 * bin_width;
        let top = egui::pos2(x, rect.bottom() - bar_height);
        let bottom = egui::pos2(x + bin_width.max(1.0), rect.bottom());
        painter.rect_filled(
            egui::Rect::from_min_max(top, bottom),
            0.0,
            Color32::from_rgba_unmultiplied(140, 200, 255, 210),
        );
    }

    if let Some(cur_path) = current {
        if let Some(p) = points.iter().find(|p| p.path.as_path() == cur_path) {
            let frac = ((p.score - min) / range).clamp(0.0, 1.0);
            let x = rect.left() + frac * rect.width();
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(2.0, Color32::from_rgb(255, 210, 90)),
            );
        }
    }

    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Color32::from_gray(80)),
        egui::StrokeKind::Inside,
    );
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

    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, Color32::from_gray(80)),
        egui::StrokeKind::Inside,
    );
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

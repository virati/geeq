//! kgallery: every video of a folder, playing together in an NxN grid.
//!
//! * One libmpv instance per tile, rendered into its own FBO texture inside
//!   an egui glow paint callback, then blitted into the tile's rectangle.
//! * Every tile loops its own video. The demuxer keeps the clip in memory
//!   (`demuxer-max-back-bytes`), so the loop seek never goes back to disk.
//! * A page (NxN tiles) loads paused. Once every tile has decoded its first
//!   frame, all tiles are un-paused in one pass, so the grid starts in step.
//! * The next and the previous page load in the background, paused on frame
//!   0, and keep rendering into their FBOs (through a 1x1 paint callback), so
//!   paging is instant. A page still loading never replaces the one shown.
//! * GL objects are only freed inside a paint callback (the only place the GL
//!   context is known to be current): dropped tiles wait in `grave`.
//!
//! Memory is bounded: at most 3 pages of at most `MAX_N`x`MAX_N` tiles, each
//! with a capped demuxer cache.

use eframe::{egui, egui_glow, glow};
use glow::HasContext as _;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_N: usize = 6;
const DEFAULT_MAX_N: usize = 4;
/// A page that is still not ready after this long starts anyway.
const START_TIMEOUT: Duration = Duration::from_secs(20);
const GAP: f32 = 2.0;

mod ffi {
    use super::*;

    pub enum Handle {}
    pub enum RenderCtx {}

    #[repr(C)]
    pub struct Event {
        pub event_id: c_int,
        pub error: c_int,
        pub reply_userdata: u64,
        pub data: *mut c_void,
    }
    #[repr(C)]
    pub struct EventProperty {
        pub name: *const c_char,
        pub format: c_int,
        pub data: *mut c_void,
    }
    #[repr(C)]
    pub struct EventEndFile {
        pub reason: c_int,
        pub error: c_int,
    }
    #[repr(C)]
    pub struct RenderParam {
        pub kind: c_int,
        pub data: *mut c_void,
    }
    #[repr(C)]
    pub struct GlInitParams {
        pub get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
        pub ctx: *mut c_void,
    }
    #[repr(C)]
    pub struct Fbo {
        pub fbo: c_int,
        pub w: c_int,
        pub h: c_int,
        pub internal_format: c_int,
    }

    pub const EVENT_NONE: c_int = 0;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_PLAYBACK_RESTART: c_int = 21;
    pub const EVENT_PROPERTY_CHANGE: c_int = 22;
    pub const FORMAT_FLAG: c_int = 3;
    pub const END_FILE_REASON_ERROR: c_int = 4;
    pub const RENDER_PARAM_INVALID: c_int = 0;
    pub const RENDER_PARAM_API_TYPE: c_int = 1;
    pub const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
    pub const RENDER_PARAM_OPENGL_FBO: c_int = 3;
    pub const RENDER_PARAM_FLIP_Y: c_int = 4;
    pub const RENDER_UPDATE_FRAME: u64 = 1;

    #[link(name = "mpv")]
    extern "C" {
        pub fn mpv_create() -> *mut Handle;
        pub fn mpv_initialize(h: *mut Handle) -> c_int;
        pub fn mpv_set_option_string(h: *mut Handle, name: *const c_char, value: *const c_char) -> c_int;
        pub fn mpv_set_property_string(h: *mut Handle, name: *const c_char, value: *const c_char) -> c_int;
        pub fn mpv_command_async(h: *mut Handle, reply: u64, args: *mut *const c_char) -> c_int;
        pub fn mpv_wait_event(h: *mut Handle, timeout: f64) -> *mut Event;
        pub fn mpv_terminate_destroy(h: *mut Handle);
        pub fn mpv_observe_property(h: *mut Handle, reply: u64, name: *const c_char, format: c_int) -> c_int;
        pub fn mpv_set_wakeup_callback(h: *mut Handle, cb: Option<unsafe extern "C" fn(*mut c_void)>, d: *mut c_void);
        pub fn mpv_error_string(e: c_int) -> *const c_char;
        pub fn mpv_render_context_create(res: *mut *mut RenderCtx, mpv: *mut Handle, params: *mut RenderParam) -> c_int;
        pub fn mpv_render_context_set_update_callback(
            ctx: *mut RenderCtx,
            cb: Option<unsafe extern "C" fn(*mut c_void)>,
            d: *mut c_void,
        );
        pub fn mpv_render_context_update(ctx: *mut RenderCtx) -> u64;
        pub fn mpv_render_context_render(ctx: *mut RenderCtx, params: *mut RenderParam) -> c_int;
        pub fn mpv_render_context_free(ctx: *mut RenderCtx);
    }

    #[link(name = "EGL")]
    extern "C" {
        pub fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    }

    extern "C" {
        pub fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
}

/// mpv threads call this: ask egui for a frame.
unsafe extern "C" fn wake(data: *mut c_void) {
    let ctx = &*(data as *const egui::Context);
    ctx.request_repaint();
}

unsafe extern "C" fn get_proc_address(_: *mut c_void, name: *const c_char) -> *mut c_void {
    // eframe renders through EGL on Wayland (and normally on X11 too).
    let addr = ffi::eglGetProcAddress(name);
    if addr.is_null() {
        ffi::dlsym(std::ptr::null_mut(), name)
    } else {
        addr
    }
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

/// The mpv instance and GL objects of one tile. Touched only on the UI
/// thread (in `update`, and in paint callbacks, which run on it too).
struct GlTile {
    mpv: *mut ffi::Handle,
    rc: *mut ffi::RenderCtx,
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    size: (i32, i32),
    /// Heap copy of the egui context that mpv's callbacks point to.
    wake_ctx: *mut egui::Context,
    path: PathBuf,
    load_sent: bool,
    needs_render: bool,
    /// First frame decoded (or the tile failed).
    ready: bool,
    failed: bool,
    /// Slideshow: being watched for the end of its last play.
    armed: bool,
    /// Slideshow: played all its plays (or cannot play).
    done: bool,
}

// Raw pointers are only used from the UI thread.
unsafe impl Send for GlTile {}

impl GlTile {
    fn new(ctx: &egui::Context, path: PathBuf) -> Self {
        let mut t = GlTile {
            mpv: std::ptr::null_mut(),
            rc: std::ptr::null_mut(),
            fbo: None,
            tex: None,
            size: (0, 0),
            wake_ctx: Box::into_raw(Box::new(ctx.clone())),
            path,
            load_sent: false,
            needs_render: true,
            ready: false,
            failed: false,
            armed: false,
            done: false,
        };

        unsafe {
            let m = ffi::mpv_create();
            if m.is_null() {
                t.fail("mpv_create failed");
                return t;
            }
            let hwdec = std::env::var("GEEQ_HWDEC").unwrap_or_else(|_| "auto-safe".to_string());
            let options: [(&str, &str); 22] = [
                ("config", "no"),
                ("terminal", "no"),
                ("load-scripts", "no"),
                ("input-default-bindings", "no"),
                ("input-vo-keyboard", "no"),
                ("osc", "no"),
                ("ytdl", "no"),
                ("vo", "libmpv"),
                ("hwdec", &hwdec),
                ("aid", "no"),
                ("sid", "no"),
                ("pause", "yes"),
                ("loop-file", "inf"),
                ("keep-open", "yes"),
                ("hr-seek", "yes"),
                ("cache", "yes"),
                // Keep the clip itself in memory so the loop seek never touches the disk.
                ("demuxer-max-bytes", "96MiB"),
                ("demuxer-max-back-bytes", "96MiB"),
                ("demuxer-readahead-secs", "600"),
                ("idle", "yes"), // else mpv quits before the file is loaded
                ("really-quiet", "yes"),
                ("keepaspect", "yes"),
            ];
            for (k, v) in options {
                let (k, v) = (cstr(k), cstr(v));
                if ffi::mpv_set_option_string(m, k.as_ptr(), v.as_ptr()) < 0 {
                    eprintln!("kgallery: mpv option {k:?}={v:?} refused");
                }
            }
            let err = ffi::mpv_initialize(m);
            if err < 0 {
                ffi::mpv_terminate_destroy(m);
                t.fail(&format!("mpv_initialize: {}", CStr::from_ptr(ffi::mpv_error_string(err)).to_string_lossy()));
                return t;
            }
            // eof-reached turns true after the last of the finite loops (keep-open holds the last frame).
            ffi::mpv_observe_property(m, 0, c"eof-reached".as_ptr(), ffi::FORMAT_FLAG);
            ffi::mpv_set_wakeup_callback(m, Some(wake), t.wake_ctx as *mut c_void);
            t.mpv = m;
        }
        t
    }

    fn fail(&mut self, why: &str) {
        eprintln!("kgallery: {}: {why}", self.path.display());
        self.failed = true;
        self.ready = true;
        self.done = true; // a broken file must not hold the slideshow back
    }

    fn set_property(&self, name: &str, value: &str) {
        if self.mpv.is_null() {
            return;
        }
        let (n, v) = (cstr(name), cstr(value));
        unsafe { ffi::mpv_set_property_string(self.mpv, n.as_ptr(), v.as_ptr()) };
    }

    fn set_paused(&self, paused: bool) {
        self.set_property("pause", if paused { "yes" } else { "no" });
    }

    /// Play the file `plays` times in all, or forever when 0 (loop-file=N repeats N times).
    fn set_plays(&mut self, plays: u32) {
        self.armed = plays > 0;
        self.done = self.failed;
        let value = if plays > 0 { (plays - 1).to_string() } else { "inf".to_string() };
        self.set_property("loop-file", &value);
    }

    fn rewind(&self) {
        if self.mpv.is_null() {
            return;
        }
        let args = [cstr("seek"), cstr("0"), cstr("absolute+exact")];
        let mut ptrs: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { ffi::mpv_command_async(self.mpv, 0, ptrs.as_mut_ptr()) };
    }

    /// Drain mpv's events (UI thread, from `update`).
    fn pump(&mut self) {
        while !self.mpv.is_null() {
            let ev = unsafe { &*ffi::mpv_wait_event(self.mpv, 0.0) };
            if ev.event_id != ffi::EVENT_NONE && std::env::var_os("KGALLERY_DEBUG").is_some() {
                eprintln!("kgallery: event {} err {} {}", ev.event_id, ev.error, self.path.display());
            }
            match ev.event_id {
                ffi::EVENT_NONE => break,
                ffi::EVENT_PLAYBACK_RESTART => self.ready = true,
                ffi::EVENT_PROPERTY_CHANGE if !ev.data.is_null() => {
                    let prop = unsafe { &*(ev.data as *const ffi::EventProperty) };
                    let is_eof = !prop.name.is_null() && unsafe { CStr::from_ptr(prop.name) }.to_bytes() == b"eof-reached";
                    if is_eof && prop.format == ffi::FORMAT_FLAG && !prop.data.is_null() {
                        if unsafe { *(prop.data as *const c_int) } != 0 && self.armed {
                            self.done = true;
                        }
                    }
                }
                ffi::EVENT_END_FILE if !ev.data.is_null() => {
                    let end = unsafe { &*(ev.data as *const ffi::EventEndFile) };
                    if end.reason == ffi::END_FILE_REASON_ERROR {
                        let why = unsafe { CStr::from_ptr(ffi::mpv_error_string(end.error)) };
                        let why = why.to_string_lossy().into_owned();
                        self.fail(&why); // a broken file must not hold the page back
                    }
                }
                _ => {}
            }
        }
    }

    /// Create the render context and start loading. Needs the GL context current.
    unsafe fn init_render(&mut self) {
        if !self.rc.is_null() || self.mpv.is_null() {
            return;
        }
        let mut init = ffi::GlInitParams { get_proc_address: Some(get_proc_address), ctx: std::ptr::null_mut() };
        let mut params = [
            ffi::RenderParam { kind: ffi::RENDER_PARAM_API_TYPE, data: c"opengl".as_ptr() as *mut c_void },
            ffi::RenderParam { kind: ffi::RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut _ as *mut c_void },
            ffi::RenderParam { kind: ffi::RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        let err = ffi::mpv_render_context_create(&mut self.rc, self.mpv, params.as_mut_ptr());
        if err < 0 {
            self.rc = std::ptr::null_mut();
            let why = CStr::from_ptr(ffi::mpv_error_string(err)).to_string_lossy().into_owned();
            self.fail(&format!("render context: {why}"));
            return;
        }
        ffi::mpv_render_context_set_update_callback(self.rc, Some(wake), self.wake_ctx as *mut c_void);

        if !self.load_sent {
            self.load_sent = true;
            let path = cstr(&self.path.to_string_lossy());
            let args = [cstr("loadfile"), path, cstr("replace")];
            let mut ptrs: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            ffi::mpv_command_async(self.mpv, 0, ptrs.as_mut_ptr());
        }
    }

    unsafe fn ensure_target(&mut self, gl: &glow::Context, size: (i32, i32)) {
        if self.fbo.is_some() && self.size == size {
            return;
        }
        self.free_target(gl);
        let Ok(tex) = gl.create_texture() else { return };
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D, 0, glow::RGBA8 as i32, size.0, size.1, 0,
            glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None),
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
        let Ok(fbo) = gl.create_framebuffer() else { return };
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        self.tex = Some(tex);
        self.fbo = Some(fbo);
        self.size = size;
        self.needs_render = true;
    }

    unsafe fn free_target(&mut self, gl: &glow::Context) {
        if let Some(f) = self.fbo.take() {
            gl.delete_framebuffer(f);
        }
        if let Some(t) = self.tex.take() {
            gl.delete_texture(t);
        }
    }

    /// Render a new mpv frame (if there is one) into the FBO, and blit the FBO
    /// into `blit` (a viewport on the screen) when the tile is visible.
    unsafe fn paint(
        &mut self,
        gl: &glow::Context,
        screen_fbo: Option<glow::Framebuffer>,
        size: (i32, i32),
        blit: Option<egui::epaint::ViewportInPixels>,
    ) {
        self.init_render();
        if self.rc.is_null() || size.0 <= 0 || size.1 <= 0 {
            return;
        }
        self.ensure_target(gl, size);
        let Some(fbo) = self.fbo else { return };

        let flags = ffi::mpv_render_context_update(self.rc);
        if flags & ffi::RENDER_UPDATE_FRAME != 0 || self.needs_render {
            self.needs_render = false;
            let mut target = ffi::Fbo { fbo: fbo.0.get() as c_int, w: size.0, h: size.1, internal_format: 0 };
            let mut flip_y: c_int = 1;
            let mut params = [
                ffi::RenderParam { kind: ffi::RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut _ as *mut c_void },
                ffi::RenderParam { kind: ffi::RENDER_PARAM_FLIP_Y, data: &mut flip_y as *mut _ as *mut c_void },
                ffi::RenderParam { kind: ffi::RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
            ];
            let err = ffi::mpv_render_context_render(self.rc, params.as_mut_ptr());
            if std::env::var_os("KGALLERY_DEBUG").is_some() {
                eprintln!("kgallery: render {} -> {err} flags={flags} size={size:?} visible={}", self.path.display(), blit.is_some());
            }
        }

        if let Some(vp) = blit {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, screen_fbo);
            gl.blit_framebuffer(
                0, 0, size.0, size.1,
                vp.left_px, vp.from_bottom_px, vp.left_px + size.0, vp.from_bottom_px + size.1,
                glow::COLOR_BUFFER_BIT, glow::NEAREST,
            );
        }
        gl.bind_framebuffer(glow::FRAMEBUFFER, screen_fbo);
    }

    /// Free everything. Needs the GL context current.
    unsafe fn destroy(&mut self, gl: &glow::Context) {
        if !self.rc.is_null() {
            ffi::mpv_render_context_set_update_callback(self.rc, None, std::ptr::null_mut());
            ffi::mpv_render_context_free(self.rc);
            self.rc = std::ptr::null_mut();
        }
        if !self.mpv.is_null() {
            ffi::mpv_set_wakeup_callback(self.mpv, None, std::ptr::null_mut());
            ffi::mpv_terminate_destroy(self.mpv);
            self.mpv = std::ptr::null_mut();
        }
        self.free_target(gl);
        if !self.wake_ctx.is_null() {
            drop(Box::from_raw(self.wake_ctx));
            self.wake_ctx = std::ptr::null_mut();
        }
    }
}

type SharedTile = Arc<Mutex<GlTile>>;

struct Page {
    index: usize,
    tiles: Vec<SharedTile>,
    started: bool,
    created: Instant,
}

impl Page {
    fn all_ready(&self) -> bool {
        self.tiles.iter().all(|t| t.lock().map(|t| t.ready).unwrap_or(true))
    }
}

pub enum Outcome {
    Open,
    Closed,
}

pub struct KGallery {
    ctx: egui::Context,
    dir: PathBuf,
    files: Vec<PathBuf>,
    n: usize,
    /// Page the user asked for.
    cur: usize,
    /// Page on screen.
    shown: Option<usize>,
    pages: Vec<Page>,
    paused: bool,
    fullscreen: bool,
    /// Slideshow: each page plays every video `loops` times, waits `interval`
    /// seconds after the last play, then moves to the next page.
    show: bool,
    loops: u32,
    interval: f32,
    advance_at: Option<Instant>,
    /// Tiles waiting for a paint callback to free their GL objects.
    grave: Arc<Mutex<Vec<SharedTile>>>,
}

impl KGallery {
    /// None when the folder has no videos.
    pub fn new(ctx: &egui::Context, dir: PathBuf, files: Vec<PathBuf>) -> Option<Self> {
        if files.is_empty() {
            return None;
        }
        let n = ((files.len() as f64).sqrt().ceil() as usize).clamp(1, DEFAULT_MAX_N);
        Some(KGallery {
            ctx: ctx.clone(),
            dir,
            files,
            n,
            cur: 0,
            shown: None,
            pages: Vec::new(),
            paused: false,
            fullscreen: false,
            show: false,
            loops: 2,
            interval: 2.0,
            advance_at: None,
            grave: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn page_count(&self) -> usize {
        let per = self.n * self.n;
        self.files.len().div_ceil(per).max(1)
    }

    fn find_page(&self, index: usize) -> Option<usize> {
        self.pages.iter().position(|p| p.index == index)
    }

    fn new_page(&self, index: usize) -> Page {
        let per = self.n * self.n;
        let tiles = self
            .files
            .iter()
            .skip(index * per)
            .take(per)
            .map(|f| Arc::new(Mutex::new(GlTile::new(&self.ctx, f.clone()))))
            .collect();
        Page { index, tiles, started: false, created: Instant::now() }
    }

    fn bury(&self, page: Page) {
        if let Ok(mut grave) = self.grave.lock() {
            grave.extend(page.tiles);
        }
    }

    /// Load the wanted page and its neighbours; drop the others (never the one on screen).
    fn trim(&mut self) {
        let count = self.page_count();
        let wanted = [self.cur, (self.cur + 1) % count, (self.cur + count - 1) % count];

        for w in wanted {
            if self.find_page(w).is_none() {
                let page = self.new_page(w);
                self.pages.push(page);
            }
        }
        let shown = self.shown;
        let (keep, drop): (Vec<Page>, Vec<Page>) =
            std::mem::take(&mut self.pages).into_iter().partition(|p| Some(p.index) == shown || wanted.contains(&p.index));
        self.pages = keep;
        for p in drop {
            self.bury(p);
        }
    }

    fn goto(&mut self, page: isize) {
        self.advance_at = None;
        let count = self.page_count() as isize;
        self.cur = page.rem_euclid(count) as usize;
        if let Some(i) = self.find_page(self.cur) {
            if self.shown == Some(self.cur) {
                return;
            }
            // A page that was left (and reset to frame 0) starts again once it is ready.
            self.pages[i].started = false;
        }
        self.trim();
    }

    fn set_n(&mut self, n: usize) {
        let n = n.clamp(1, MAX_N);
        if n == self.n {
            return;
        }
        let first_file = self.cur * self.n * self.n;
        for p in std::mem::take(&mut self.pages) {
            self.bury(p);
        }
        self.shown = None;
        self.n = n;
        self.goto((first_file / (n * n)) as isize);
    }

    /// Make the wanted page the visible one if it is ready: stop the others
    /// and start this one's tiles in one pass.
    fn present(&mut self) {
        let Some(i) = self.find_page(self.cur) else { return };
        if self.shown == Some(self.cur) {
            return;
        }
        for (j, page) in self.pages.iter_mut().enumerate() {
            if j == i {
                continue;
            }
            for t in &page.tiles {
                if let Ok(mut t) = t.lock() {
                    t.set_plays(0);
                    t.set_paused(true);
                    t.rewind();
                }
            }
            page.started = false;
        }
        let (paused, plays) = (self.paused, if self.show { self.loops } else { 0 });
        for t in &self.pages[i].tiles {
            if let Ok(mut t) = t.lock() {
                t.set_plays(plays);
            }
        }
        for t in &self.pages[i].tiles {
            if let Ok(t) = t.lock() {
                t.set_paused(paused);
            }
        }
        self.pages[i].started = true;
        self.shown = Some(self.cur);
        self.trim();
    }

    fn set_show(&mut self, on: bool) {
        self.show = on;
        self.advance_at = None;
        let (paused, plays) = (self.paused, if on { self.loops } else { 0 });
        if let Some(i) = self.shown.and_then(|s| self.find_page(s)) {
            for t in &self.pages[i].tiles {
                if let Ok(mut t) = t.lock() {
                    t.set_plays(plays);
                    // Tiles that stopped on their last frame play on, looping.
                    if !on {
                        t.rewind();
                        t.set_paused(paused);
                    }
                }
            }
        }
    }

    /// Slideshow timer: once every tile of the shown page played all its
    /// plays, wait `interval`, then go to the next page (or replay a single page).
    fn tick_show(&mut self, ctx: &egui::Context) {
        if !self.show {
            return;
        }
        let Some(i) = self.shown.and_then(|s| self.find_page(s)) else { return };

        if self.advance_at.is_none() {
            let all_done = self.pages[i].tiles.iter().all(|t| t.lock().map(|t| t.done).unwrap_or(true));
            if all_done {
                self.advance_at = Some(Instant::now() + Duration::from_secs_f32(self.interval));
            }
        }
        let Some(at) = self.advance_at else { return };
        let now = Instant::now();
        if now < at {
            ctx.request_repaint_after(at - now);
            return;
        }
        self.advance_at = None;

        if self.page_count() > 1 {
            self.goto(self.cur as isize + 1);
        } else {
            let (paused, plays) = (self.paused, self.loops);
            for t in &self.pages[i].tiles {
                if let Ok(mut t) = t.lock() {
                    t.set_plays(plays);
                    t.rewind();
                    t.set_paused(paused);
                }
            }
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) -> bool {
        use egui::Key;
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));

        if pressed(Key::Escape) || pressed(Key::Q) {
            return false;
        }
        if pressed(Key::ArrowRight) || pressed(Key::PageDown) || pressed(Key::N) {
            self.goto(self.cur as isize + 1);
        }
        if pressed(Key::ArrowLeft) || pressed(Key::PageUp) || pressed(Key::P) {
            self.goto(self.cur as isize - 1);
        }
        if pressed(Key::Home) {
            self.goto(0);
        }
        if pressed(Key::Plus) || pressed(Key::Equals) {
            self.set_n(self.n + 1);
        }
        if pressed(Key::Minus) {
            self.set_n(self.n.saturating_sub(1));
        }
        if pressed(Key::Space) {
            self.paused = !self.paused;
            if let Some(i) = self.shown.and_then(|s| self.find_page(s)) {
                for t in &self.pages[i].tiles {
                    if let Ok(t) = t.lock() {
                        t.set_paused(self.paused);
                    }
                }
            }
        }
        if pressed(Key::S) {
            self.set_show(!self.show);
        }
        if pressed(Key::OpenBracket) {
            self.loops = self.loops.saturating_sub(1).max(1);
        }
        if pressed(Key::CloseBracket) {
            self.loops += 1;
        }
        if pressed(Key::Comma) {
            self.interval = (self.interval - 0.5).max(0.0);
        }
        if pressed(Key::Period) {
            self.interval = (self.interval + 0.5).min(3600.0);
        }
        if pressed(Key::F) || pressed(Key::F11) {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        true
    }

    /// One frame. Draws the grid into `ui`; returns `Closed` once the user quit.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Outcome {
        let ctx = ui.ctx().clone();
        let keep_open = self.handle_keys(&ctx);

        if keep_open {
            self.trim();
        }

        // Events, readiness, start.
        for page in &self.pages {
            for t in &page.tiles {
                if let Ok(mut t) = t.lock() {
                    t.pump();
                }
            }
        }
        if keep_open {
            if let Some(i) = self.find_page(self.cur) {
                let page = &self.pages[i];
                let ready = page.all_ready() || page.created.elapsed() > START_TIMEOUT;
                if ready && self.shown != Some(self.cur) {
                    self.present();
                }
            }
        }

        if keep_open {
            self.tick_show(&ctx);
        }

        // Layout.
        let rect = ui.available_rect_before_wrap();
        let ppp = ctx.pixels_per_point();
        let n = self.n as f32;
        let cell = egui::vec2((rect.width() - GAP * (n - 1.0)) / n, (rect.height() - GAP * (n - 1.0)) / n);
        let px = ((cell.x * ppp).round() as i32, (cell.y * ppp).round() as i32);
        ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);

        let shown = self.shown;
        if keep_open {
            for page in &self.pages {
                for (i, tile) in page.tiles.iter().enumerate() {
                    let visible = Some(page.index) == shown;
                    let pos = rect.min + egui::vec2((i % self.n) as f32 * (cell.x + GAP), (i / self.n) as f32 * (cell.y + GAP));
                    // A page that is not shown still needs paint calls to keep decoding: a 1x1 rect does that.
                    let tile_rect = if visible { egui::Rect::from_min_size(pos, cell) } else { egui::Rect::from_min_size(pos, egui::vec2(1.0, 1.0)) };
                    let tile = tile.clone();
                    let callback = egui_glow::CallbackFn::new(move |info, painter| {
                        let Ok(mut t) = tile.lock() else { return };
                        let blit = if visible { Some(info.viewport_in_pixels()) } else { None };
                        unsafe { t.paint(painter.gl(), painter.intermediate_fbo(), px, blit) };
                    });
                    ui.painter().add(egui::PaintCallback { rect: tile_rect, callback: Arc::new(callback) });
                }
            }
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        } else {
            for p in std::mem::take(&mut self.pages) {
                self.bury(p);
            }
        }

        // Free dropped tiles inside a paint callback, where the GL context is current.
        let grave = self.grave.clone();
        let cleanup = egui_glow::CallbackFn::new(move |_info, painter| {
            let Ok(mut grave) = grave.lock() else { return };
            for tile in grave.drain(..) {
                if let Ok(mut t) = tile.lock() {
                    unsafe { t.destroy(painter.gl()) };
                }
            }
        });
        ui.painter().add(egui::PaintCallback {
            rect: egui::Rect::from_min_size(rect.min, egui::vec2(1.0, 1.0)),
            callback: Arc::new(cleanup),
        });

        let show = if self.show { format!(" - slideshow {}x, {:.1}s", self.loops, self.interval) } else { String::new() };
        let page_title = format!(
            "kgallery {n}x{n} - page {}/{}{show} - {}",
            self.shown.unwrap_or(0) + 1,
            self.page_count(),
            self.dir.display(),
            n = self.n
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(page_title));

        if keep_open {
            Outcome::Open
        } else {
            Outcome::Closed
        }
    }
}

//! picoSun2 viewer — one file. Folder scan, decode cache, egui UI with strip.
//! Feature parity with the picoSun session, minus the bugs we paid for.

use eframe::egui;
use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_millis(220);
const NOTCH: f32 = 40.0; // egui Options::line_scroll_speed: one wheel notch in points
const FADE: f32 = 0.18; // crossfade seconds
const TILE: f32 = 49.0; // Picasa strip tile height
const BAR: f32 = 17.0; // info bar above the strip

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "webp", "bmp", "dib", "tif", "tiff", "ico",
    "cur", "tga", "ppm", "pgm", "pbm", "pnm", "qoi", "dds", "hdr", "exr", "avif", "jp2",
    "jxl", "heic", "heif", "hif", "psd", "pcx", "svg", "svgz", "xpm", "xbm", "psb",
    // RAW: listed so folders sort right; decode falls through with a clear error.
    "arw", "cr2", "cr3", "crw", "dng", "nef", "nrw", "orf", "raf", "rw2", "raw", "erf",
    "3fr", "fff", "gpr", "iiq", "kdc", "mrw", "pef", "rwl", "srw", "x3f", "bay", "cap",
    "dcr", "dcs", "k25", "mdc", "mef", "mos", "ptx", "pxn", "r3d", "sr2", "srf", "ari",
    "drf", "eip", "obm", "rwz",
];

fn is_image(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Explorer-style natural sort: digit runs compare numerically (pic2 < pic10).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    // ponytail: digit-run chunks only; close enough to Explorer without a locale table
    let (mut ia, mut ib) = (0usize, 0usize);
    let (ba, bb) = (a.as_bytes(), b.as_bytes());
    while ia < ba.len() && ib < bb.len() {
        if ba[ia].is_ascii_digit() && bb[ib].is_ascii_digit() {
            let na = ba[ia..].iter().take_while(|c| c.is_ascii_digit()).count();
            let nb = bb[ib..].iter().take_while(|c| c.is_ascii_digit()).count();
            let va: u64 =
                std::str::from_utf8(&ba[ia..ia + na]).unwrap_or("").parse().unwrap_or(0);
            let vb: u64 =
                std::str::from_utf8(&bb[ib..ib + nb]).unwrap_or("").parse().unwrap_or(0);
            if va != vb {
                return va.cmp(&vb);
            }
            ia += na;
            ib += nb;
        } else {
            match ba[ia].to_ascii_lowercase().cmp(&bb[ib].to_ascii_lowercase()) {
                std::cmp::Ordering::Equal => {
                    ia += 1;
                    ib += 1;
                }
                o => return o,
            }
        }
    }
    (ba.len() - ia).cmp(&(bb.len() - ib))
}

#[derive(Default, PartialEq, Clone, Copy)]
enum ZoomMode {
    #[default]
    Fit,
    Fill,
    Width,
    Height,
    Actual,
}

struct Zoom {
    mode: ZoomMode,
    scale: f32,
    offset: egui::Vec2,
    locked: bool, // Lock Zoom (L): a landing keeps the user's scale
}

impl Default for Zoom {
    fn default() -> Self {
        // derive(Default) would give scale=0.0 (f32 default) → fit_rect
        // multiplies by 0 → zero-size rect → photo invisible (the "black photos")
        Self {
            mode: ZoomMode::Fit,
            scale: 1.0,
            offset: egui::Vec2::ZERO,
            locked: false,
        }
    }
}

pub struct App {
    folder: Vec<PathBuf>,
    index: usize,
    tex: Option<egui::TextureHandle>,      // settled photo
    prev_tex: Option<egui::TextureHandle>, // fading-out photo (old fades OUT on top)
    fade: Option<Instant>,
    zoom: Zoom,
    error: Option<String>,
    pick_mode: bool, // Color Picker armed (K)
    rot: u8,         // 0/1/2/3 × 90° (R key, session: rotate)
    flipped: bool,   // H key flips horizontally
    frame: usize,    // multi-frame index (GIF etc.)
    // wheel paging: bank notches during a burst; settle lands the last one
    banked: i32,
    wheel_points: f32, // strip scroll: accumulate egui's smoothed points, step per notch
    last_notch: Instant,
    strip_hover: bool,
    strip_on: bool,     // Ctrl+T shows/hides the strip (old viewer parity)
    drag_accum: f32,    // pixels dragged this press: click toggles only if tiny
    thumbs: HashMap<PathBuf, egui::TextureHandle>,
    decoding: Option<PathBuf>,
    tile_rx: Receiver<(PathBuf, Result<Arc<DynamicImage>, String>)>,
    tile_tx: Sender<(PathBuf, Result<Arc<DynamicImage>, String>)>,
    master_rx: Receiver<(PathBuf, Result<Arc<DynamicImage>, String>)>,
    master_tx: Sender<(PathBuf, Result<Arc<DynamicImage>, String>)>,
    master_tex: Vec<(PathBuf, egui::TextureHandle)>, // uploaded masters, LRU-8
    info_size: u64, // file size for the info bar: stat once, not per frame
    last_ctx: Option<egui::Context>, // stashed each update for retexture()
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, start: Option<String>) -> Self {
        // thumb decodes can saturate rayon's global pool — leave one core
        // for the UI thread (build_global fails harmlessly if already built)
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(
                std::thread::available_parallelism()
                    .map(|n| n.get().saturating_sub(1).max(1))
                    .unwrap_or(3),
            )
            .build_global();
        let (tile_tx, tile_rx) = channel();
        let (master_tx, master_rx) = channel();
        let mut app = Self {
            folder: vec![],
            index: 0,
            tex: None,
            prev_tex: None,
            fade: None,
            zoom: Zoom::default(),
            error: None,
            pick_mode: false,
            rot: 0,
            flipped: false,
            frame: 0,
            banked: 0,
            wheel_points: 0.0,
            last_notch: Instant::now(),
            strip_hover: false,
            strip_on: true,
            drag_accum: 0.0,
            thumbs: HashMap::new(),
            decoding: None,
            tile_rx,
            tile_tx,
            master_rx,
            master_tx,
            master_tex: vec![],
            info_size: 0,
            last_ctx: None,
        };
        if let Some(p) = start {
            app.open(&p);
        }
        app
    }

    // ------------------------------------------------------------------ state

    fn current(&self) -> Option<&Path> {
        self.folder.get(self.index).map(|p| p.as_path())
    }

    /// Re-read the folder, Explorer order, photos only.
    fn rescan(&mut self, anchor: &Path) {
        let dir = anchor.parent().unwrap_or(Path::new("."));
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_image(p))
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by(|a, b| {
            natural_cmp(
                &a.file_name().unwrap_or_default().to_string_lossy(),
                &b.file_name().unwrap_or_default().to_string_lossy(),
            )
        });
        self.folder = files;
        self.index = self.folder.iter().position(|p| p == anchor).unwrap_or(0);
    }

    /// Explicit open (arg, double-click, drop): instant-first.
    fn open(&mut self, path: &str) {
        let path = match std::fs::canonicalize(path) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(format!("{}: {}", path, e));
                return;
            }
        };
        if !path.is_file() {
            self.error = Some(format!("not a file: {}", path.display()));
            return;
        }
        self.rescan(&path);
        self.show_current();
    }

    /// Show the current index: fit landing, master decode queued on worker.
    fn show_current(&mut self) {
        let Some(p) = self.current().map(|p| p.to_path_buf()) else { return };
        self.error = None;
        self.info_size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if !self.zoom.locked {
            // landing = fit view, fresh pan (Lock Zoom keeps the user's scale)
            self.zoom.mode = ZoomMode::Fit;
            self.zoom.scale = 1.0;
            self.zoom.offset = egui::Vec2::ZERO;
        }
        if !self.thumbs.contains_key(&p) {
            self.queue_thumb(p.clone());
        }
        if let Some(h) = self.cached_master(&p) {
            // uploaded already this session: land instantly, no decode wait
            self.decoding = None;
            self.prev_tex = self.tex.take();
            self.tex = Some(h);
            self.fade = Some(Instant::now());
            return;
        }
        self.decoding = Some(p.clone());
        spawn_master(p, self.master_tx.clone());
    }

    /// Uploaded master textures: paging back shows the photo at once
    /// (no re-decode, no re-upload — the per-landing stutter).
    fn cached_master(&self, p: &Path) -> Option<egui::TextureHandle> {
        self.master_tex.iter().find(|(q, _)| q == p).map(|(_, h)| h.clone())
    }

    fn put_master(&mut self, p: PathBuf, h: egui::TextureHandle) {
        self.master_tex.retain(|(q, _)| q != &p);
        if self.master_tex.len() >= 8 {
            self.master_tex.remove(0); // LRU-8: bounded VRAM (~8 masters)
        }
        self.master_tex.push((p, h));
    }

    /// Decode one small tile on the worker pool (never blocks the UI).
    fn queue_thumb(&mut self, path: PathBuf) {
        if self.thumbs.contains_key(&path) {
            return;
        }
        let tx = self.tile_tx.clone();
        rayon::spawn(move || {
            let img = read_image(&path)
                .map(|im| Arc::new(im.thumbnail((TILE * 3.0) as u32, (TILE * 3.0) as u32)));
            let _ = tx.send((path, img.map_err(|e| e.to_string())));
        });
    }

    fn step(&mut self, delta: i32) {
        if self.folder.len() < 2 {
            return;
        }
        let n = self.folder.len() as i32;
        let next = (self.index as i32 + delta).clamp(0, n - 1) as usize;
        if next != self.index {
            self.index = next; // hard stop at folder edges
            self.show_current();
        }
    }

    /// Wheel paging, session contract: one notch = one photo, hard stop at
    /// edges. Each notch moves instantly on the cached tile; the full master
    /// decodes only once the wheel settles (see update()).
    fn wheel(&mut self, delta: i32) {
        if self.folder.len() < 2 {
            return;
        }
        self.wheel_step(delta.signum());
        self.banked += delta;
        self.last_notch = Instant::now();
    }

    /// Light step: move the index and show the cached tile now (~0ms).
    /// No master decode here — the settle handler queues exactly one.
    fn wheel_step(&mut self, delta: i32) {
        let n = self.folder.len() as i32;
        let next = (self.index as i32 + delta).clamp(0, n - 1) as usize;
        if next == self.index {
            return; // hard stop at folder edges
        }
        self.index = next;
        self.error = None;
        if !self.zoom.locked {
            self.zoom.mode = ZoomMode::Fit;
            self.zoom.scale = 1.0;
            self.zoom.offset = egui::Vec2::ZERO;
        }
        let p = self.folder[self.index].clone();
        self.info_size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if !self.thumbs.contains_key(&p) {
            self.queue_thumb(p.clone());
        } else if let Some(tile) = self.thumbs.get(&p).cloned() {
            // instant feedback: show the tile until the master lands
            self.prev_tex = None;
            self.tex = Some(tile);
            self.fade = None;
        }
    }

    /// Queue exactly one master decode for wherever the wheel settled.
    fn settle_master(&mut self) {
        let Some(p) = self.current().map(|p| p.to_path_buf()) else { return };
        if self.decoding.as_deref() == Some(p.as_path()) {
            return; // already in flight
        }
        if let Some(h) = self.cached_master(&p) {
            // seen this session: sharpen instantly (tile → master crossfade)
            self.decoding = None;
            self.prev_tex = self.tex.take();
            self.tex = Some(h);
            self.fade = Some(Instant::now());
            return;
        }
        self.decoding = Some(p.clone());
        spawn_master(p, self.master_tx.clone());
    }

    /// Take tiles and masters off their worker channels; upload as textures.
    fn poll_decode(&mut self, ctx: &egui::Context) {
        while let Ok((path, res)) = self.tile_rx.try_recv() {
            match res {
                Ok(img) => {
                    let tex = upload(ctx, &path, &img);
                    // tile for the photo we're now on = instant feedback while
                    // its master still decodes (cold cache showed nothing)
                    let is_current = self.current() == Some(path.as_path());
                    if is_current && self.decoding.is_some() && self.fade.is_none() {
                        self.prev_tex = None;
                        self.tex = Some(tex.clone());
                    }
                    self.thumbs.insert(path, tex);
                }
                Err(_) => {} // a bad tile just stays blank in the strip
            }
        }
        while let Ok((path, res)) = self.master_rx.try_recv() {
            // stale master (user paged past it already) must not clobber
            if self.decoding.as_deref() != Some(path.as_path()) {
                continue;
            }
            match res {
                Ok(img) => {
                    let tex = upload_master(ctx, &path, img);
                    self.put_master(path.clone(), tex.clone());
                    // crossfade: old photo stays on top and fades OUT
                    self.prev_tex = self.tex.take();
                    self.tex = Some(tex);
                    self.fade = Some(Instant::now());
                    self.decoding = None;
                }
                Err(e) => {
                    self.error = Some(format!("{}: {}", path.display(), e));
                    self.decoding = None;
                }
            }
        }
    }
}

/// Masters decode on their OWN single thread: a thumb storm on the global
/// pool must never delay the landing sharpen (session: instant-first).
fn spawn_master(
    path: PathBuf,
    tx: Sender<(PathBuf, Result<Arc<DynamicImage>, String>)>,
) {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    let pool = POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .thread_name(|i| format!("p2-master-{i}"))
            .build()
            .expect("master pool")
    });
    pool.spawn(move || {
        let img = read_image(&path).map(Arc::new);
        let _ = tx.send((path, img));
    });
}

fn read_image(path: &Path) -> Result<DynamicImage, String> {
    image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// Count frames of a GIF via its logical screen + image descriptors (cheap).
fn count_frames(path: &Path) -> i64 {
    // ponytail: header scan only — good enough to enable/disable frame nav
    match std::fs::read(path) {
        Ok(b) => b.iter().filter(|&&x| x == 0x21).count() as i64,
        Err(_) => 1,
    }
}

fn to_color_image(img: &DynamicImage) -> egui::ColorImage {
    let rgba = img.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as _, img.height() as _],
        rgba.as_raw(),
    )
}

fn upload(ctx: &egui::Context, path: &Path, img: &DynamicImage) -> egui::TextureHandle {
    ctx.load_texture(path.display().to_string(), to_color_image(img), Default::default())
}

/// Master upload: CPU pixels Arc-shared into the cache (no copy), texture
/// on screen, and the handle also kept by the caller for master_tex.
fn upload_master(
    ctx: &egui::Context,
    path: &Path,
    img: Arc<DynamicImage>,
) -> egui::TextureHandle {
    if let Ok(mut c) = pixel_cache().lock() {
        c.insert(path.to_path_buf(), img.clone()); // Arc clone = pointer copy
    }
    upload(ctx, path, &img)
}

/// masters cache: decoded DynamicImage per path (session: master_for).
/// Lets Color Picker read real pixels; Arc-shared with the decode worker.
fn pixel_cache() -> &'static std::sync::Mutex<HashMap<PathBuf, Arc<DynamicImage>>> {
    static CELL: std::sync::OnceLock<Mutex<HashMap<PathBuf, Arc<DynamicImage>>>> =
        std::sync::OnceLock::new();
    CELL.get_or_init(|| Mutex::new(HashMap::new()))
}

impl App {
    /// Save the current (rot/flip-applied) master to disk, format by extension.
    fn save_current(&mut self) {
        let Some(path) = self.current().map(|p| p.to_path_buf()) else { return };
        let Some(img) = pixel_cache().lock().ok().and_then(|c| c.get(&path).cloned()) else {
            self.error = Some("nothing loaded".into());
            return;
        };
        let mut img = (*img).clone();
        match self.rot {
            1 => img = img.rotate90(),
            2 => img = img.rotate180(),
            3 => img = img.rotate270(),
            _ => {}
        }
        if self.flipped {
            img = img.fliph();
        }
        match img.save(&path) {
            Ok(_) => self.error = Some("saved".into()),
            Err(e) => self.error = Some(format!("save failed: {e}")),
        }
    }

    /// Resize the master in place (factor <1 shrinks), save, refresh cache.
    fn resize_master(&mut self, factor: f32) {
        let Some(path) = self.current().map(|p| p.to_path_buf()) else { return };
        let Some(img) = pixel_cache().lock().ok().and_then(|c| c.get(&path).cloned()) else { return };
        let w = (img.width() as f32 * factor) as u32;
        let h = (img.height() as f32 * factor) as u32;
        let img = Arc::new(img.resize(w, h, image::imageops::FilterType::Lanczos3));
        if let Ok(mut c) = pixel_cache().lock() {
            c.insert(path.clone(), img.clone());
        }
        self.master_tex.retain(|(q, _)| q != &path); // texture cache: stale now
        match img.save(&path) {
            Ok(_) => {
                self.retexture();
                self.error = Some(format!("resized to {}×{}", w, h));
            }
            Err(e) => self.error = Some(format!("resize failed: {e}")),
        }
    }

    /// Step one frame of a multi-frame image; re-decodes from the master.
    fn next_frame(&mut self, dir: i64) {
        let Some(path) = self.current().map(|p| p.to_path_buf()) else { return };
        let Some(master) = pixel_cache().lock().ok().and_then(|c| c.get(&path).cloned()) else { return };
        // ponytail: `image` crate decodes only the first GIF frame; real
        // multi-frame needs the `gif` decoder — counts frames via seek loop
        let frames = count_frames(&path);
        if frames < 2 {
            return;
        }
        self.frame = (self.frame as i64 + dir).rem_euclid(frames) as usize;
        self.retexture();
    }

    /// Rebuild the current texture from the cached master with rot/flip.
    fn retexture(&mut self) {
        let Some(path) = self.current().map(|p| p.to_path_buf()) else { return };
        let Some(img) = pixel_cache().lock().ok().and_then(|c| c.get(&path).cloned()) else { return };
        let mut img = (*img).clone();
        match self.rot {
            1 => img = img.rotate90(),
            2 => img = img.rotate180(),
            3 => img = img.rotate270(),
            _ => {}
        }
        if self.flipped {
            img = img.fliph();
        }
        if let Some(ctx) = self.ctx() {
            self.tex = Some(upload(&ctx, &path, &img));
            self.fade = None;
            self.prev_tex = None;
        }
    }

    fn ctx(&self) -> Option<egui::Context> {
        // ponytail: egui has no stored Context on App; update() passes it in.
        // We stash the latest on first update instead.
        self.last_ctx.clone()
    }

    /// Read one pixel of the current master (u,v in 0..1 view space).
    fn pixel_at(&self, _id: egui::TextureId, u: f32, v: f32) -> Option<[u8; 3]> {
        let path = self.decoding.clone()?;
        let cache = pixel_cache().lock().ok()?;
        let img = cache.get(&path)?;
        let rgba = img.to_rgba8();
        let x = ((rgba.width() as f32 * u) as u32).min(rgba.width() - 1);
        let y = ((rgba.height() as f32 * v).min(rgba.height() as f32 - 1.0)) as u32;
        let p = rgba.get_pixel(x as u32, y as u32);
        Some([p[0], p[1], p[2]])
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.last_ctx = Some(ctx.clone()); // retexture() needs it between frames
        self.poll_decode(ctx);

        // fade finished → drop the old photo (otherwise repaint runs forever)
        if let Some(t) = self.fade {
            if t.elapsed().as_secs_f32() >= FADE {
                self.fade = None;
                self.prev_tex = None;
            }
        }

        // settle: wheel quiet → decode the master for wherever we landed.
        // Steps already moved per-notch on cached tiles; exactly one master.
        if self.banked != 0 && self.last_notch.elapsed() >= SETTLE {
            self.banked = 0;
            self.settle_master();
        }

        // input: fullscreen / pages / zoom / pan
        if ctx.input(|i| (i.key_pressed(egui::Key::F) && !i.modifiers.shift) || i.key_pressed(egui::Key::F11)) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        // Esc: leave fullscreen, or close when already fitted (old parity)
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if fs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else if !self.is_fitted() {
                self.set_mode(ZoomMode::Fit);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Lock Zoom (L): a landing keeps the user's scale (session rule)
        if ctx.input(|i| i.key_pressed(egui::Key::L)) {
            self.zoom.locked = !self.zoom.locked;
        }
        // rotate (R): re-render the master transformed, same path as the old
        // viewer's non-destructive edits. Flip moved to Ctrl+H (H = height).
        if ctx.input(|i| i.key_pressed(egui::Key::R) && !i.modifiers.ctrl) {
            self.rot = (self.rot + 1) % 4;
            self.retexture();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::H) && i.modifiers.ctrl) {
            self.flipped = !self.flipped;
            self.retexture();
        }
        // one-shot view modes (old nav parity): 0 fit / 1 actual / W width /
        // H height / Shift+F fill / + − anchored zoom
        if ctx.input(|i| i.key_pressed(egui::Key::Num0)) {
            self.set_mode(ZoomMode::Fit);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Num1)) {
            self.set_mode(ZoomMode::Actual);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::W) && !i.modifiers.ctrl) {
            self.set_mode(ZoomMode::Width);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::H) && !i.modifiers.ctrl) {
            self.set_mode(ZoomMode::Height);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F) && i.modifiers.shift) {
            self.set_mode(ZoomMode::Fill);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::T) && i.modifiers.ctrl) {
            self.strip_on = !self.strip_on; // Ctrl+T toggles the strip
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Equals)) {
            self.zoom_key(ui_rect(ctx), 1.0, ctx.input(|i| i.pointer.latest_pos()));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Minus)) {
            self.zoom_key(ui_rect(ctx), -1.0, ctx.input(|i| i.pointer.latest_pos()));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Space)) {
            self.step(1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::PageDown)) {
            self.step(1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::PageUp)) {
            self.step(-1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Backspace)) {
            self.step(-1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Home)) {
            if !self.folder.is_empty() {
                self.index = 0;
                self.show_current();
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::End)) {
            let n = self.folder.len();
            if n > 0 {
                self.index = n - 1;
                self.show_current();
            }
        }

        // wheel: ON the photo → anchored zoom; over the letterbox, the bottom
        // bar or the strip → page (old viewer rule; strip claims the wheel
        // only once hovered). Ctrl+wheel zooms too (egui's Event::Zoom).
        let area = ui_rect(ctx);
        let img_size = self.tex.as_ref().map(|t| t.size_vec2());
        let photo_rect = img_size.map(|s| fit_rect(area, s, &self.zoom));
        let (wheel_y, pos, strip_hot, drag_delta) =
            ctx.input(|i| (i.smooth_scroll_delta.y, i.pointer.latest_pos(), self.strip_hover, i.pointer.delta()));
        let on_photo = pos.is_some_and(|p| photo_rect.is_some_and(|r| r.contains(p)));
        let ctrl_zoom = ctx.input(|i| i.zoom_delta());
        if ctrl_zoom != 1.0 && img_size.is_some() {
            // one event per notch (no frame smoothing): direction is enough
            let f = if ctrl_zoom > 1.0 { 1.15 } else { 1.0 / 1.15 };
            let pt = if on_photo { pos.unwrap() } else { area.center() };
            zoom_step(&mut self.zoom, area, img_size.unwrap(), f, pt);
        }
        if wheel_y != 0.0 {
            if strip_hot || !on_photo {
                // egui smooths one wheel notch (~40pt) across ~10 frames, so
                // stepping per frame jumped ~10 photos. Accumulate, step/notch.
                self.wheel_points += wheel_y;
                let notch = NOTCH;
                while self.wheel_points <= -notch {
                    self.wheel_points += notch;
                    self.wheel(1);
                }
                while self.wheel_points >= notch {
                    self.wheel_points -= notch;
                    self.wheel(-1);
                }
            } else {
                // zoom: same smoothing rule as the strip — egui spreads one
                // notch over ~10 frames; accumulate, one step per notch.
                self.wheel_points += wheel_y;
                while self.wheel_points.abs() >= NOTCH {
                    let up = self.wheel_points > 0.0;
                    self.wheel_points = if up { self.wheel_points - NOTCH } else { self.wheel_points + NOTCH };
                    let f = if up { 1.15 } else { 1.0 / 1.15 };
                    if let (Some(s), Some(pt)) = (img_size, pos) {
                        zoom_step(&mut self.zoom, area, s, f, pt);
                    }
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            self.step(1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            self.step(-1);
        }
        // drag pan (clamped per-axis like the old viewer; drag distance also
        // suppresses the click-to-toggle that follows a real drag)
        if ctx.input(|i| i.pointer.primary_down()) {
            self.zoom.offset += drag_delta;
            self.drag_accum += drag_delta.length();
            if let Some(s) = img_size {
                clamp_view(&mut self.zoom, area, s);
            }
        }

        // drag & drop a photo → open it (session: resolve_target semantics)
        if ctx.input(|i| !i.raw.dropped_files.is_empty()) {
            if let Some(f) = ctx.input(|i| i.raw.dropped_files.clone()).first() {
                if let Some(p) = &f.path {
                    self.open(&p.to_string_lossy());
                }
            }
        }

        // frame navigation for multi-frame images (GIF/WebP/APNG): . / , keys
        if ctx.input(|i| i.key_pressed(egui::Key::Period)) {
            self.next_frame(1);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Comma)) {
            self.frame = self.frame.saturating_sub(1);
            self.retexture();
        }

        // Ctrl+S: save the current view (rot/flip applied) back to disk
        if ctx.input(|i| (i.key_pressed(egui::Key::S) && i.modifiers.ctrl)) {
            self.save_current();
        }
        // Ctrl+R: resize dialog is a clipboard-free inline prompt: Ctrl+Shift+R
        // resizes the master to 50% and saves (session: resize-on-save, lazy)
        if ctx.input(|i| (i.key_pressed(egui::Key::R) && i.modifiers.ctrl && i.modifiers.shift)) {
            self.resize_master(0.5);
        }

        // Color Picker: K arms it, next click copies the pixel color
        if ctx.input(|i| i.key_pressed(egui::Key::K)) {
            self.pick_mode = !self.pick_mode;
        }
        let pick_armed = self.pick_mode; // click below must not also toggle fit
        if self.pick_mode {
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                if ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(tex) = &self.tex {
                        let area = ui_rect(ctx);
                        let rect = fit_rect(area, tex.size_vec2(), &self.zoom);
                        if rect.contains(pos) {
                            // map window point → texture pixel
                            let u = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                            let v = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                            if let Some(px) = self.pixel_at(tex.id(), u, v) {
                                let hex = format!("#{:02X}{:02X}{:02X}", px[0], px[1], px[2]);
                                if let Ok(mut cb) = arboard::Clipboard::new() {
                                    let _ = cb.set_text(&hex);
                                }
                                self.error = Some(format!("copied {}", hex)); // reuse the status line
                                self.pick_mode = false;
                            }
                        }
                    }
                }
            }
        }

        // click on the photo: toggle fit ↔ 100%; double-click: leave
        // fullscreen first, then the same toggle (old viewer parity).
        let dbl = ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
        let clicked = ctx.input(|i| i.pointer.primary_clicked());
        if dbl || clicked {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            let tiny_drag = self.drag_accum < 3.0;
            self.drag_accum = 0.0;
            let on = on_photo && !strip_hot; // a strip tile click opens, never toggles
            if dbl {
                if fs {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                } else if on && tiny_drag && !pick_armed {
                    self.toggle_fit();
                }
            } else if on && tiny_drag && !pick_armed {
                self.toggle_fit();
            }
        }

        // the whole window is one layer: photo floats, desktop shows through
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| self.paint(ui));

        // fade animating → keep painting
        if self.fade.is_some() {
            ctx.request_repaint();
        }

        // egui stops repainting when input stops, but the wheel settle and the
        // worker-channel results need FUTURE frames — otherwise the master
        // decode after a page never lands (tex stayed the 147px tile forever)
        if self.banked != 0 {
            ctx.request_repaint_after(SETTLE + Duration::from_millis(32));
        }
        if self.decoding.is_some() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

fn ui_input(ctx: &egui::Context) -> (f32, bool, bool) {
    ctx.input(|i| {
        (
            i.smooth_scroll_delta.y,
            false, // strip hover resolved inside paint; wheel paging uses explicit hover
            false,
        )
    })
}

/// The central panel rect without building a Ui (for input math).
fn ui_rect(ctx: &egui::Context) -> egui::Rect {
    ctx.screen_rect()
}

impl App {
    /// The photo is fitted with no pan (Esc close-when-fitted, click toggle).
    fn is_fitted(&self) -> bool {
        self.zoom.mode == ZoomMode::Fit
            && (self.zoom.scale - 1.0).abs() < 1e-3
            && self.zoom.offset == egui::Vec2::ZERO
    }

    /// One-shot view modes: 0 fit / 1 actual / W width / H height / Shift+F fill.
    fn set_mode(&mut self, mode: ZoomMode) {
        self.zoom.mode = mode;
        self.zoom.scale = 1.0;
        self.zoom.offset = egui::Vec2::ZERO;
    }

    /// click / double-click: toggle fit ↔ 100% (old viewer parity).
    fn toggle_fit(&mut self) {
        if self.is_fitted() {
            self.set_mode(ZoomMode::Actual);
        } else {
            self.set_mode(ZoomMode::Fit);
        }
    }

    /// +/− keys: zoom anchored at the pointer when it is on the photo,
    /// from the centre otherwise (old nav-bar behaviour).
    fn zoom_key(&mut self, area: egui::Rect, dir: f32, pos: Option<egui::Pos2>) {
        let Some(size) = self.tex.as_ref().map(|t| t.size_vec2()) else {
            return;
        };
        let f = if dir > 0.0 { 1.15 } else { 1.0 / 1.15 };
        let pt = match pos {
            Some(p) if fit_rect(area, size, &self.zoom).contains(p) => p,
            _ => area.center(),
        };
        zoom_step(&mut self.zoom, area, size, f, pt);
    }

    /// One layer: photo floats over nothing; bars draw on top.
    fn paint(&mut self, ui: &mut egui::Ui) {
        let area = ui.max_rect();
        // Painter is a cheap handle (Arc inside); owning it frees `ui` for
        // allocate_rect calls later in the same function.
        let painter = ui.painter().clone();
        // The window owns the mouse everywhere: one fill of alpha 1/255 —
        // invisible over any wallpaper, but hit-tests to us (the old viewer
        // measured 64% of its own window belonging to the browser behind).
        painter.rect_filled(area, 0.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 1));

        // photo + crossfade (old fades OUT on top — session rule)
        if let Some(tex) = self.tex.clone() {
            let size = tex.size_vec2();
            let rect = fit_rect(area, size, &self.zoom);
            // soft drop shadow: the photo floats on the letterbox
            for (pad, a) in [(6.0, 60u8), (14.0, 34), (26.0, 16)] {
                painter.rect_filled(rect.expand(pad), 4.0, egui::Color32::from_black_alpha(a));
            }
            let mut tint = egui::Color32::WHITE;
            if let Some(t) = self.fade {
                let a = ((t.elapsed().as_secs_f32() / FADE).min(1.0) * 255.0) as u8;
                tint = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 255 - a as u8);
            }
            if let Some(prev) = self.prev_tex.clone() {
                if tint.a() > 0 {
                    let ps = prev.size_vec2();
                    painter.image(
                        prev.id(),
                        fit_rect(area, ps, &self.zoom),
                        uv(),
                        tint,
                    );
                }
            }
            painter.image(tex.id(), rect, uv(), egui::Color32::WHITE);
        }

        // Picasa strip: navy band at the bottom (Ctrl+T hides the whole row)
        let strip_h = if self.strip_on { TILE } else { 0.0 };
        let strip_rect = egui::Rect::from_min_size(
            egui::pos2(area.left(), area.bottom() - strip_h),
            egui::vec2(area.width(), strip_h),
        );
        if self.strip_on {
            painter.rect_filled(strip_rect, 0.0, egui::Color32::from_rgb(8, 9, 12));
        }

        // floating ⤢ top-right: only in fullscreen; leaves fullscreen, never quits
        let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        if fs {
            let btn = egui::Rect::from_min_size(
                egui::pos2(area.right() - 48.0, 8.0),
                egui::vec2(40.0, 30.0),
            );
            let resp = ui.allocate_rect(btn, egui::Sense::click());
            painter.rect_filled(btn, 8.0, if resp.hovered() {
                egui::Color32::from_rgb(196, 43, 28)
            } else {
                egui::Color32::from_rgba_premultiplied(16, 20, 30, 120)
            });
            painter.text(
                btn.center(),
                egui::Align2::CENTER_CENTER,
                "⤢",
                egui::FontId::proportional(15.0),
                egui::Color32::WHITE,
            );
            if resp.clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        }

        // info bar above (name + counter), thin grey
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(area.left(), strip_rect.top() - BAR),
            egui::vec2(area.width(), BAR),
        );
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_rgb(24, 25, 28));
        if let Some(p) = self.current() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            // info: counter (left) · dims + size (center) · name (right)
            let dims = self.tex.as_ref().map(|t| {
                let s = t.size_vec2();
                format!("{}×{}", s.x as u32, s.y as u32)
            }).unwrap_or_default();
            let size = self.info_size; // stat once per photo, not per frame
            let kb = if size > 1_048_576 {
                format!("{:.1} MB", size as f64 / 1_048_576.0)
            } else {
                format!("{} KB", size / 1024)
            };
            painter.text(
                bar_rect.left_top() + egui::vec2(8.0, BAR / 2.0),
                egui::Align2::LEFT_CENTER,
                format!("{} / {}", self.index + 1, self.folder.len()),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(200, 200, 205),
            );
            painter.text(
                bar_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}  ·  {}", dims, kb),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(150, 152, 158),
            );
            painter.text(
                bar_rect.right_center() - egui::vec2(8.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                &name,
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(200, 200, 200),
            );
        }

        // tiles — collect hover/clicks, then act (borrow-checker friendly)
        let mut hot = false;
        let mut clicked: Option<usize> = None;
        let count = if self.strip_on { self.folder.len() } else { 0 };
        // tiles advance by their true width (aspect-preserved), like Picasa
        let mut x = area.left() + 8.0;
        let mut start = 0usize;
        // keep the current tile in view: shift the row when it lands off-screen
        if count > 0 {
            let wid = |t: &egui::TextureHandle| {
                let s = t.size_vec2();
                (s.x * (TILE / s.y)).clamp(20.0, TILE * 2.0)
            };
            let mut pos = x;
            for i in 0..self.index {
                if let Some(t) = self.thumbs.get(&self.folder[i]) {
                    pos += wid(t) + 2.0;
                }
            }
            let cur = self.thumbs.get(&self.folder[self.index]).map(wid).unwrap_or(60.0);
            if pos + cur > area.right() - 8.0 {
                let over = pos + cur - (area.right() - 8.0);
                x -= over;
                // draw only tiles that stay on screen after the shift:
                // walk back until the previous tile is fully left of the view
                let limit = area.right() - 8.0 - cur; // == pos - over
                let mut i = self.index;
                let mut acc = 0.0;
                while i > 0 && acc < limit {
                    let Some(t) = self.thumbs.get(&self.folder[i - 1]) else { break };
                    acc += wid(t) + 2.0;
                    i -= 1;
                }
                start = i;
            }
        }
        for i in start..count {
            let Some(tex) = self.thumbs.get(&self.folder[i]) else { continue };
            let sz = tex.size_vec2();
            let w = (sz.x * (TILE / sz.y)).clamp(20.0, TILE * 2.0);
            let r = egui::Rect::from_min_size(egui::pos2(x, strip_rect.top()), egui::vec2(w, TILE));
            painter.image(tex.id(), r, uv(), egui::Color32::WHITE);
            if i == self.index {
                painter.rect_stroke(r, 0.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(47, 127, 196)), egui::StrokeKind::Inside);
            }
            let resp = ui.allocate_rect(r, egui::Sense::click());
            if resp.hovered() {
                hot = true;
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            x += w + 2.0;
            if x > area.right() {
                break; // beyond the screen: skip the rest (they're off-view)
            }
        }
        self.strip_hover = hot;
        if let Some(i) = clicked {
            self.index = i;
            self.show_current();
        }

        // preload neighbours: after landing, prev/next tiles decode in the
        // background so paging feels instant (session: hardware-aware caching)
        if let Some(p) = self.current().map(|p| p.to_path_buf()) {
            for d in [-1i64, 1] {
                if let Some(n) = self.folder.get((self.index as i64 + d) as usize) {
                    if !self.thumbs.contains_key(n) {
                        self.queue_thumb(n.clone());
                    }
                }
            }
            let _ = p;
        }

        if let Some(e) = &self.error.clone() {
            painter.text(area.center(), egui::Align2::CENTER_CENTER, e,
                egui::FontId::proportional(14.0), egui::Color32::from_rgb(230, 120, 120));
        }
        if self.tex.is_none() && self.error.is_none() {
            painter.text(area.center(), egui::Align2::CENTER_CENTER,
                "No image — drop a photo here · Ctrl+O",
                egui::FontId::proportional(14.0), egui::Color32::from_rgb(226, 229, 236));
        }
    }
}

/// Full-texture UV rect. egui expects NORMALIZED [0,1] UVs — pixel-sized
/// ones (0..width) clamp to the edge, so the whole photo rendered as a single
/// color (the bottom-right pixel) and tiles looked monochrome.
fn uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
}

/// Fit/fill/width/height/actual rect for a texture inside the view area.
fn fit_rect(area: egui::Rect, size: egui::Vec2, z: &Zoom) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 || area.is_negative() {
        return area;
    }
    let k = k_of(z, area, size);
    let r = egui::Rect::from_center_size(area.center(), size * k);
    egui::Rect::from_min_max(r.min + z.offset, r.max + z.offset)
}

/// The magnification the zoom state currently implies (img px → view px).
fn k_of(z: &Zoom, area: egui::Rect, size: egui::Vec2) -> f32 {
    let (sx, sy) = (area.width() / size.x, area.height() / size.y);
    let k = match z.mode {
        ZoomMode::Fill => sx.max(sy),
        ZoomMode::Width => sx,
        ZoomMode::Height => sy,
        ZoomMode::Actual => 1.0,
        ZoomMode::Fit => sx.min(sy),
    };
    k * z.scale
}

/// The old viewer's pan rule: an axis where the image FITS is centred
/// (there is nothing to pan to); an axis where it is BIGGER pans freely so
/// the anchor stays honoured exactly ("free while pannable, centred once
/// the image fits" — test_zoom_anchor).
fn clamp_view(z: &mut Zoom, area: egui::Rect, size: egui::Vec2) {
    if size.x <= 0.0 || area.is_negative() {
        return;
    }
    let ext = size * k_of(z, area, size);
    if ext.x <= area.width() {
        z.offset.x = 0.0;
    }
    if ext.y <= area.height() {
        z.offset.y = 0.0;
    }
}

/// One wheel/±zoom step anchored at `pt`: the image point under the cursor
/// stays under it (old viewer rule — test_zoom_anchor ported as zoom_anchor).
/// The fit→free transition re-bases `scale` to the CURRENT effective k first,
/// so switching modes never jumps the magnification.
fn zoom_step(z: &mut Zoom, area: egui::Rect, size: egui::Vec2, f: f32, pt: egui::Pos2) {
    if size.x <= 0.0 || area.is_negative() || f <= 0.0 {
        return;
    }
    let old = fit_rect(area, size, z);
    if old.width() <= 0.0 {
        return;
    }
    z.mode = ZoomMode::Actual;
    z.scale = (old.width() / size.x) * f;
    let new = fit_rect(area, size, z);
    let rx = new.width() / old.width();
    let ry = new.height() / old.height();
    z.offset += egui::vec2(
        pt.x - (pt.x - old.min.x) * rx - new.min.x,
        pt.y - (pt.y - old.min.y) * ry - new.min.y,
    );
    clamp_view(z, area, size);
}

fn vp(b: bool) -> bool {
    b
}

#[cfg(test)]
mod decode_tests {
    use super::*;

    #[test]
    fn decode_pic1() {
        let p = std::path::Path::new(r"Z:\hermes\picasa-photo-viewer\testpics\pic1.jpg");
        let img = read_image(p).expect("decode failed");
        println!("dims: {}x{}", img.width(), img.height());
        let rgba = img.to_rgba8();
        for (x, y, label) in [
            (0usize, 0usize, "topleft"),
            (800, 500, "center"),
            (1599, 999, "botright"),
            (800, 10, "toprow"),
        ] {
            let px = rgba.get_pixel(x as u32, y as u32);
            println!("{label} ({x},{y}): {:?}", px.0);
        }
        let mut sum = [0u64; 3];
        let mut n = 0u64;
        for py in rgba.chunks_exact(4).step_by(137) {
            sum[0] += py[0] as u64;
            sum[1] += py[1] as u64;
            sum[2] += py[2] as u64;
            n += 1;
        }
        println!("mean: ({},{},{}) over {} samples", sum[0] / n, sum[1] / n, sum[2] / n, n);
        // PIL reference: dims 1600x1000, topleft ~61, mean (58,53,72)
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    const VIEW: (f32, f32) = (1000.0, 700.0);
    const IMG: (f32, f32) = (4000.0, 3000.0);

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(VIEW.0, VIEW.1))
    }
    fn img() -> egui::Vec2 {
        egui::vec2(IMG.0, IMG.1)
    }
    /// The image-space point currently under `pt` (the old test's img_point).
    fn img_point(z: &Zoom, pt: egui::Pos2) -> egui::Pos2 {
        let r = fit_rect(area(), img(), z);
        egui::pos2(
            (pt.x - r.min.x) / (r.width() / IMG.0),
            (pt.y - r.min.y) / (r.height() / IMG.1),
        )
    }
    fn drift(a: egui::Pos2, b: egui::Pos2) -> f32 {
        (a.x - b.x).abs().max((a.y - b.y).abs())
    }

    #[test]
    fn wheel_zoom_is_anchored_at_the_cursor() {
        let mut z = Zoom::default(); // fit
        let pt = egui::pos2(250.0, 200.0); // well off-centre: drift would show
        let k0 = k_of(&z, area(), img());
        let p0 = img_point(&z, pt);
        for _ in 0..8 {
            zoom_step(&mut z, area(), img(), 1.15, pt);
        }
        let k1 = k_of(&z, area(), img());
        assert!(
            (k1 / k0 - 1.15f32.powi(8)).abs() < 1e-3,
            "fit→free must not jump the magnification: k {k0} → {k1}"
        );
        assert!(drift(img_point(&z, pt), p0) < 0.5, "wheel-in anchored");
        // zoom back out while the image still fills the view: still anchored
        for _ in 0..4 {
            zoom_step(&mut z, area(), img(), 1.0 / 1.15, pt);
        }
        assert!(drift(img_point(&z, pt), p0) < 0.5, "wheel-out anchored");
        // a different anchor must give a different result (anchor drives it)
        let mut a = Zoom::default();
        let mut b = Zoom::default();
        zoom_step(&mut a, area(), img(), 1.15, egui::pos2(200.0, 150.0));
        zoom_step(&mut b, area(), img(), 1.15, egui::pos2(800.0, 550.0));
        assert!(
            (fit_rect(area(), img(), &a).left() - fit_rect(area(), img(), &b).left()).abs() > 1.0,
            "the anchor point actually drives the result"
        );
    }

    #[test]
    fn axes_clamp_independently() {
        // tiny: centred on both axes (nothing to pan to)
        let mut z = Zoom::default();
        z.mode = ZoomMode::Actual;
        z.scale = 0.02;
        z.offset = egui::vec2(500.0, -300.0);
        clamp_view(&mut z, area(), img());
        let r = fit_rect(area(), img(), &z);
        assert!(
            (r.center().x - VIEW.0 / 2.0).abs() < 1.0 && (r.center().y - VIEW.1 / 2.0).abs() < 1.0,
            "tiny zoom centres, got {:?}",
            r.center()
        );

        // bigger than the view: panning is FREE — the old viewer honours
        // the anchor exactly and never fights the drag ("free while
        // pannable, centred once the image fits")
        z.scale = 2.0;
        z.offset = egui::vec2(77.0, -55.0);
        clamp_view(&mut z, area(), img());
        assert!(
            z.offset == egui::vec2(77.0, -55.0),
            "bigger axis pans freely, got {:?}",
            z.offset
        );

        // wider than the view but shorter: free horizontally, centred
        // vertically — the two axes decide alone
        let wide = egui::vec2(4000.0, 2000.0);
        let mut z = Zoom::default();
        z.mode = ZoomMode::Actual;
        z.scale = 0.26; // → 1040×520 in 1000×700
        z.offset = egui::vec2(777.0, -555.0);
        clamp_view(&mut z, area(), wide);
        assert!(z.offset.x == 777.0, "wide axis stays free: {}", z.offset.x);
        assert!(z.offset.y == 0.0, "short axis centred: {}", z.offset.y);
        let r = fit_rect(area(), wide, &z);
        assert!(
            (r.center().y - VIEW.1 / 2.0).abs() < 1.0,
            "short axis centred: {:?}",
            r.center()
        );
    }

    #[test]
    fn a_fitted_axis_cannot_be_panned() {
        // fit view: every axis fits → a drag is reset, the photo cannot be
        // lost in the letterbox (the dizzying drift the old notes warn about)
        let mut z = Zoom::default(); // 933×700 fitted into 1000×700
        z.offset = egui::vec2(40.0, -30.0);
        clamp_view(&mut z, area(), img());
        assert!(
            z.offset == egui::Vec2::ZERO,
            "fitted photo centres, got {:?}",
            z.offset
        );
    }
}


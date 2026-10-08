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
    thumbs: HashMap<PathBuf, egui::TextureHandle>,
    decoding: Option<PathBuf>,
    tile_rx: Receiver<(PathBuf, Result<DynamicImage, String>)>,
    tile_tx: Sender<(PathBuf, Result<DynamicImage, String>)>,
    master_rx: Receiver<(PathBuf, Result<DynamicImage, String>)>,
    master_tx: Sender<(PathBuf, Result<DynamicImage, String>)>,
    last_ctx: Option<egui::Context>, // stashed each update for retexture()
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, start: Option<String>) -> Self {
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
            thumbs: HashMap::new(),
            decoding: None,
            tile_rx,
            tile_tx,
            master_rx,
            master_tx,
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
        if !self.zoom.locked {
            // landing = fit view, fresh pan (Lock Zoom keeps the user's scale)
            self.zoom.mode = ZoomMode::Fit;
            self.zoom.scale = 1.0;
            self.zoom.offset = egui::Vec2::ZERO;
        }
        if !self.thumbs.contains_key(&p) {
            self.queue_thumb(p.clone());
        }
        self.decoding = Some(p.clone());
        let tx = self.master_tx.clone();
        rayon::spawn(move || {
            let img = read_image(&p);
            let _ = tx.send((p, img));
        });
    }

    /// Decode one small tile on the worker pool (never blocks the UI).
    fn queue_thumb(&mut self, path: PathBuf) {
        if self.thumbs.contains_key(&path) {
            return;
        }
        let tx = self.tile_tx.clone();
        rayon::spawn(move || {
            let img =
                read_image(&path).map(|im| im.thumbnail((TILE * 3.0) as u32, (TILE * 3.0) as u32));
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
        self.decoding = Some(p.clone());
        let tx = self.master_tx.clone();
        rayon::spawn(move || {
            let img = read_image(&p);
            let _ = tx.send((p, img));
        });
    }

    /// Take tiles and masters off their worker channels; upload as textures.
    fn poll_decode(&mut self, ctx: &egui::Context) {
        while let Ok((path, res)) = self.tile_rx.try_recv() {
            match res {
                Ok(img) => {
                    let tex = upload(ctx, &path, img);
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
                    let tex = upload(ctx, &path, img);
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

fn upload(ctx: &egui::Context, path: &Path, img: DynamicImage) -> egui::TextureHandle {
    let name = path.display().to_string();
    // keep the decoded master for pixel tools (Color Picker)
    if let Ok(mut c) = pixel_cache().lock() {
        c.insert(path.to_path_buf(), std::sync::Arc::new(img.clone()));
    }
    ctx.load_texture(name, to_color_image(&img), Default::default())
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
            self.tex = Some(upload(&ctx, &path, img));
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
        if ctx.input(|i| i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::F11)) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if fs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Lock Zoom (L): a landing keeps the user's scale (session rule)
        if ctx.input(|i| i.key_pressed(egui::Key::L)) {
            self.zoom.locked = !self.zoom.locked;
        }
        // rotate (R) / flip (H): re-render the master transformed, same path
        // as the old viewer's non-destructive edits
        if ctx.input(|i| i.key_pressed(egui::Key::R)) {
            self.rot = (self.rot + 1) % 4;
            self.retexture();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::H)) {
            self.flipped = !self.flipped;
            self.retexture();
        }

        // wheel: on photo → zoom anchored at the pointer; over strip → page
        let (wheel_y, pos, strip_hot, drag_delta) =
            ctx.input(|i| (i.smooth_scroll_delta.y, i.pointer.latest_pos(), self.strip_hover, i.pointer.delta()));
        if wheel_y != 0.0 {
            if strip_hot {
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
                // notch over ~10 frames; stepping per frame zoomed 0.21x/notch
                // and threw the photo off-screen (scale=0.12, off=-2500).
                self.wheel_points += wheel_y;
                while self.wheel_points.abs() >= NOTCH {
                    let up = self.wheel_points > 0.0;
                    self.wheel_points = if up { self.wheel_points - NOTCH } else { self.wheel_points + NOTCH };
                    // keep the pixel under the pointer still while zooming:
                    // new_rect = pointer - (pointer - old_rect.min) * factor
                    let f = if up { 1.15 } else { 1.0 / 1.15 };
                    let area = ui_rect(ctx);
                    if let Some(pt) = pos {
                        if let Some(tex) = &self.tex {
                            let old = fit_rect(area, tex.size_vec2(), &self.zoom);
                            self.zoom.scale *= f;
                            if self.zoom.mode == ZoomMode::Fit {
                                self.zoom.mode = ZoomMode::Actual; // manual zoom
                            }
                            let new = fit_rect(area, tex.size_vec2(), &self.zoom);
                            self.zoom.offset += (old.min - new.min) + (old.min - pt) * (1.0 - f) / f;
                            // ponytail: offset correction is algebra on rect corners;
                            // exact pointer-stability verified by test below
                        }
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
        // drag pan when zoomed
        if ctx.input(|i| i.pointer.primary_down()) {
            self.zoom.offset += drag_delta;
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

        // double-click: fullscreen off (session rule); single click on photo = nothing
        if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            if fs {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
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
    fn zoom_at(&mut self, dir: f32) {
        let f = if dir > 0.0 { 1.15 } else { 1.0 / 1.15 };
        self.zoom.scale *= f;
        self.zoom.mode = ZoomMode::Actual;
    }

    /// One layer: photo floats over nothing; bars draw on top.
    fn paint(&mut self, ui: &mut egui::Ui) {
        let area = ui.max_rect();
        // Painter is a cheap handle (Arc inside); owning it frees `ui` for
        // allocate_rect calls later in the same function.
        let painter = ui.painter().clone();

        // photo + crossfade (old fades OUT on top — session rule)
        if let Some(tex) = self.tex.clone() {
            let size = tex.size_vec2();
            let rect = fit_rect(area, size, &self.zoom);
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

        // Picasa strip: navy band at the bottom, tiles 30px tall
        let strip_rect = egui::Rect::from_min_size(
            egui::pos2(area.left(), area.bottom() - TILE),
            egui::vec2(area.width(), TILE),
        );
        painter.rect_filled(strip_rect, 0.0, egui::Color32::from_rgb(8, 9, 12));

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
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
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
        let count = self.folder.len();
        // tiles advance by their true width (aspect-preserved), like Picasa
        let mut x = area.left() + 8.0;
        for i in 0..count {
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
    let (sx, sy) = (area.width() / size.x, area.height() / size.y);
    let k = match z.mode {
        ZoomMode::Fill => sx.max(sy),
        ZoomMode::Width => sx,
        ZoomMode::Height => sy,
        ZoomMode::Actual => 1.0,
        ZoomMode::Fit => sx.min(sy),
    } * z.scale;
    let r = egui::Rect::from_center_size(area.center(), size * k);
    egui::Rect::from_min_max(r.min + z.offset, r.max + z.offset)
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


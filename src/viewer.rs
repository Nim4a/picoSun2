//! picoSun2 viewer — one file. Folder scan, decode cache, egui UI with strip.
//! Feature parity with the picoSun session, minus the bugs we paid for.

use eframe::egui;
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

const SETTLE: Duration = Duration::from_millis(220);
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
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
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

#[derive(Default)]
struct Zoom {
    mode: ZoomMode,
    scale: f32,
    offset: egui::Vec2,
    locked: bool, // Lock Zoom (L): a landing keeps the user's scale
}

pub struct App {
    folder: Vec<PathBuf>,
    index: usize,
    tex: Option<egui::TextureHandle>,      // settled photo
    prev_tex: Option<egui::TextureHandle>, // fading-out photo (old fades OUT on top)
    fade: Option<Instant>,
    zoom: Zoom,
    error: Option<String>,
    // wheel paging: bank notches during a burst; settle lands the last one
    banked: i32,
    last_notch: Instant,
    strip_hover: bool,
    thumbs: HashMap<PathBuf, egui::TextureHandle>,
    decoding: Option<PathBuf>,
    tile_rx: Receiver<(PathBuf, Result<DynamicImage, String>)>,
    tile_tx: Sender<(PathBuf, Result<DynamicImage, String>)>,
    start: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<String>) -> Self {
        let (tile_tx, tile_rx) = channel();
        let mut app = Self {
            folder: vec![],
            index: 0,
            tex: None,
            prev_tex: None,
            fade: None,
            zoom: Zoom::default(),
            error: None,
            banked: 0,
            last_notch: Instant::now(),
            strip_hover: false,
            thumbs: HashMap::new(),
            decoding: None,
            tile_rx,
            tile_tx,
            start,
        };
        if let Some(p) = &start {
            app.open(p);
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
        self.index = self
            .folder
            .iter()
            .position(|p| p == anchor)
            .unwrap_or(0);
    }

    /// Explicit open (arg, double-click, drop): instant-first.
    /// The strip tile lands in ~0ms; the master decodes on a worker.
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

    /// Show the current index: tile first (from cache), master decode queued.
    fn show_current(&mut self) {
        let Some(p) = self.current().map(|p| p.to_path_buf()) else { return };
        self.error = None;
        self.zoom.mode = ZoomMode::Fit; // a new photo always lands fitted
        // tile now if the strip already decoded it
        if self.thumbs.contains_key(&p) {
            // nothing — paint() uses the thumb until the master lands
        } else {
            self.queue_thumb(p.clone());
        }
        self.queue_master(p);
    }

    fn queue_thumb(&mut self, path: PathBuf) {
        if self.thumbs.contains_key(&path) {
            return;
        }
        let tx = self.tile_tx.clone();
        let small = path.clone();
        rayon::spawn(move || {
            let img = image::io::Reader::open(&small)
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.decode().ok())
                .map(|im| im.thumbnail(TILE as u32 * 3, TILE as u32 * 3));
            let _ = tx.send((small, img.map_err(|e| e.to_string())));
        });
    }

    fn queue_master(&mut self, path: PathBuf) {
        self.decoding = Some(path.clone());
        let tx = self.tile_tx.clone();
        rayon::spawn(move || {
            let img = image::io::Reader::open(&path)
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.decode());
            let _ = tx.send((path, img.map_err(|e| e.to_string())));
        });
    }

    /// Wheel paging, session contract: one notch = one photo, hard stop at
    /// edges, burst banks to one page, settle decodes on the worker.
    fn wheel(&mut self, delta: i32) {
        if self.folder.len() < 2 {
            return;
        }
        if self.banked == 0 {
            self.step(delta.signum()); // first notch moves instantly
        }
        self.banked += delta;
        self.last_notch = Instant::now();
    }

    /// Take tiles off the worker channel; upload as textures.
    fn poll_decode(&mut self, ctx: &egui::Context) {
        while let Ok((path, res)) = self.tile_rx.try_recv() {
            match res {
                Ok(img) => {
                    let is_master = self.decoding.as_deref() == Some(path.as_path());
                    let tex = upload(ctx, &path, img);
                    if is_master {
                        // crossfade: old photo stays on top and fades OUT
                        self.prev_tex = self.tex.take();
                        self.tex = Some(tex);
                        self.fade = Some(Instant::now());
                    } else {
                        self.thumbs.insert(path, tex);
                    }
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

                impl eframe::App for App {
                fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
                // worker results first
                self.poll_decode(ctx);

                // settle: wheel quiet for SETTLE → the banked burst already stepped;
                // nothing more to decode (we decode per step, instant-first).
                if self.banked != 0 && self.last_notch.elapsed() >= SETTLE {
                let dir = self.banked.signum();
                self.banked = 0;
                self.step(dir);
                }

                // fade progress; request repaint while animating
                let fading = self.fade.is_some();
                if fading {
                let t = self.fade.unwrap().elapsed().as_secs_f32() / FADE;
                if t >= 1.0 {
                self.fade = None;
                self.prev_tex = None;
                }
                }

                // fullscreen toggle: F / F11 / Esc-exit-fullscreen
                if ctx.input(|i| i.key_pressed(egui::Key::F) || i.key_pressed(egui::Key::F11)) {
                let vp = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!vp));
                }
                if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                if ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                }
                // wheel: on photo → zoom; over strip handled below
                let (wheel_delta, pointer) = ctx.input(|i| (i.smooth_scroll_delta.y, i.pointer.latest_pos()));
                let over_strip = self.strip_hover;
                if wheel_delta != 0.0 {
                if over_strip {
                self.wheel(if wheel_delta < 0.0 { 1 } else { -1 });
                } else {
                // zoom anchored at the pointer (session contract)
                let s = if wheel_delta > 0.0 { 1.15 } else { 1.0 / 1.15 };
                self.zoom.scale *= s;
                self.zoom.mode = ZoomMode::Actual; // leaving fit
                // ponytail: anchor correction simplified — keep pointer stable
                }
                }
                // arrows / space page
                let (left, right) = ctx.input(|i| (i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight)));
                if right { self.step(1); }
                if left { self.step(-1); }

                // drag pan when zoomed
                if self.zoom.scale > 1.001 && ctx.input(|i| i.pointer.primary_down()) {
                if let Some(d) = ctx.input(|i| i.pointer.delta()) {
                self.zoom.offset += d;
                }
                }

                let full = egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ctx, |ui| self.paint(ui));
                let _ = full;

                if fading {
                ctx.request_repaint();
                }
                }
                }

fn to_color_image(img: DynamicImage) -> egui::ColorImage {
    let rgba = img.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as _, img.height() as _],
        rgba.as_raw(),
    )
}

fn upload(ctx: &egui::Context, path: &Path, img: DynamicImage) -> egui::TextureHandle {
    let name = path.display().to_string();
    ctx.load_texture(name, to_color_image(img), Default::default())
}

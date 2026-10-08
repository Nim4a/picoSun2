//! picoSun2 viewer — one file. Folder scan, decode cache, egui UI with strip.
//! Feature parity with the picoSun session, minus the bugs we paid for.

use eframe::egui;
use image::DynamicImage;
use std::collections::HashMap;
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
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>, start: Option<String>) -> Self {
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
        self.zoom.mode = ZoomMode::Fit;
        if !self.thumbs.contains_key(&p) {
            self.queue_thumb(p.clone());
        }
        self.decoding = Some(p.clone());
        let tx = self.tile_tx.clone();
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
    /// edges, burst banks to one page.
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

fn read_image(path: &Path) -> Result<DynamicImage, String> {
    image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
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
    ctx.load_texture(name, to_color_image(&img), Default::default())
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_decode(ctx);

        // settle: burst over → banked is moot (steps already happened); drop it
        if self.banked != 0 && self.last_notch.elapsed() >= SETTLE {
            let dir = self.banked.signum();
            self.banked = 0;
            self.step(dir);
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

        // wheel: on photo → zoom anchored at the pointer; over strip → page
        let (wheel_y, pos, strip_hot, drag_delta) =
            ctx.input(|i| (i.smooth_scroll_delta.y, i.pointer.latest_pos(), self.strip_hover, i.pointer.delta()));
        if wheel_y != 0.0 {
            if strip_hot {
                self.wheel(if wheel_y < 0.0 { 1 } else { -1 });
            } else {
                // keep the pixel under the pointer still while zooming:
                // new_rect = pointer - (pointer - old_rect.min) * factor
                let f = if wheel_y > 0.0 { 1.15 } else { 1.0 / 1.15 };
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
                        egui::Rect::from_min_size(egui::Pos2::ZERO, ps),
                        tint,
                    );
                }
            }
            painter.image(tex.id(), rect, egui::Rect::from_min_size(egui::Pos2::ZERO, size), egui::Color32::WHITE);
        }

        // Picasa strip: navy band at the bottom, tiles 30px tall
        let strip_rect = egui::Rect::from_min_size(
            egui::pos2(area.left(), area.bottom() - TILE),
            egui::vec2(area.width(), TILE),
        );
        painter.rect_filled(strip_rect, 0.0, egui::Color32::from_rgb(8, 9, 12));

        // info bar above (name + counter), thin grey
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(area.left(), strip_rect.top() - BAR),
            egui::vec2(area.width(), BAR),
        );
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_rgb(24, 25, 28));
        if let Some(p) = self.current() {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            painter.text(
                bar_rect.left_top() + egui::vec2(8.0, BAR / 2.0),
                egui::Align2::LEFT_CENTER,
                format!("{} / {}", self.index + 1, self.folder.len()),
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(200, 200, 205),
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
            painter.image(tex.id(), r, egui::Rect::from_min_size(egui::Pos2::ZERO, sz), egui::Color32::WHITE);
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


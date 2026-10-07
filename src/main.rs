//! picoSun2 — Rust photo viewer. Feature port of the picoSun session:
//! instant-first open, crossfade, wheel paging/zoom, Picasa strip, fullscreen.

mod viewer;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let start = args.first().cloned();

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("picoSun2")
            .with_fullscreen(true) // every launch is fullscreen (session rule)
            .with_transparent(true) // clear glass: desktop shows through
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "picoSun2",
        opts,
        Box::new(move |cc| Ok(Box::new(viewer::App::new(cc, start)))),
    )
}

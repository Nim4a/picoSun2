# Contributing

Thanks for helping out. picoSun2 is a small Rust/egui photo viewer for Windows.

## Build

Needs Rust and the w64devkit (mingw-w64) toolchain on `PATH`.

```
cargo build --release
cargo test --release
```

The installer needs NSIS:

```
cd installer && makensis picoSun2.nsi
```

## Ground rules

- **No resolution reduction.** The master photo is always full resolution.
- **The cache lives in the photo folder.** Everything goes in `<dir>/.p2cache/` — never
  as sidecar files next to the photos, and never on `C:`.
- **The UI thread stays responsive.** Decodes run on worker pools; the UI only
  uploads finished textures.
- **Zero warnings.** `cargo clippy --release` must be clean.
- **New logic needs a test.** Add a `#[test]` next to the code it covers.

## Performance changes

Measure before and after, on a real folder, and put the numbers in the commit
message. A change that speeds up one photo but starves the filmstrip is a
regression.

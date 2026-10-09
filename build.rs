fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        // Build the icon resource with windres directly. winres ships its own
        // .lib, but rust-lld doesn't pick up its -l static=resource on the
        // GNU toolchain, so we produce the COFF and link it ourselves.
        let out = std::env::var("OUT_DIR").unwrap();
        let res = format!("{}\\app.res", out);
        let st = std::process::Command::new("windres")
            .args([
                "--input", "app.rc",
                "--output", &res,
                "--output-format", "coff",
                "--target", "pe-x86-64",
            ])
            .status()
            .expect("windres");
        assert!(st.success(), "windres failed");
        println!("cargo:rustc-link-arg={}", res);
        println!("cargo:rerun-if-changed=app.rc");
        println!("cargo:rerun-if-changed=icons/icon_main.ico");
    }
}

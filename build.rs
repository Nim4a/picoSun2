fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let mut res = winres::WindowsResource::new();
        res.set_icon("icons/icon_main.ico");
        res.compile().expect("icon resource");
    }
}

//! Embeds app.rc (the application icon) into the exe on Windows.
//! No-op elsewhere; the tray falls back to nothing icon-wise off Windows.

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let _ = embed_resource::compile("app.rc", embed_resource::NONE);
    }
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/airplay.ico");
}

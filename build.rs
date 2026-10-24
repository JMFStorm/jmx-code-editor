//! Embeds the application icon into the Windows executable. Other targets need nothing.

fn main() {
    println!("cargo:rerun-if-changed=assets/jdc.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/jdc.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("cannot embed the icon resource (is windres installed?)");
    }
}

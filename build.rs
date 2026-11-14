extern crate embed_resource;

fn main() {
    println!("cargo:rerun-if-changed=tray.rc");
    println!("cargo:rerun-if-changed=resources/app-icon.ico");
    let _ = embed_resource::compile("tray.rc", embed_resource::NONE);
}

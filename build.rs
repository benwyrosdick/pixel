//! Bundle the UI icons into a GResource that the binary embeds.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/pixel.gresource.xml");
    println!("cargo:rerun-if-changed=assets/icons");
    let target = PathBuf::from(env::var("OUT_DIR").unwrap()).join("pixel.gresource");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir", "assets/icons", "--target"])
        .arg(&target)
        .arg("assets/pixel.gresource.xml")
        .status()
        .expect("glib-compile-resources, from glib2, is needed to build Pixel");
    assert!(status.success(), "glib-compile-resources failed");
}

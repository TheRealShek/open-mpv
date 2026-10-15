use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=data/open-mpv.gresource.xml");
    println!("cargo:rerun-if-changed=data/icons");

    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"))
        .join("open-mpv.gresource");
    let status = Command::new("glib-compile-resources")
        .args(["--sourcedir=data", "--target"])
        .arg(output)
        .arg("data/open-mpv.gresource.xml")
        .status()
        .expect("glib-compile-resources is required to embed the viewer icons");
    assert!(
        status.success(),
        "could not compile the viewer icon resource"
    );
}

//! Rebuild when files of the embedded frontend change. `include_dir!` tracks
//! the files it embeds, but not files that are added to its directories.

fn main() {
    println!("cargo::rerun-if-changed=../../web/index.html");
    println!("cargo::rerun-if-changed=../../web/css");
    println!("cargo::rerun-if-changed=../../web/js");
}

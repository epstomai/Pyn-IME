fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let def_path = std::path::Path::new(&manifest_dir).join("ime_tsf.def");
    println!("cargo:rerun-if-changed={}", def_path.display());
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def_path.display());
}

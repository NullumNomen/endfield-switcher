fn main() {
    println!("cargo:rerun-if-changed=assets/endfield.ico");
    winres::WindowsResource::new()
        .set_icon("assets/endfield.ico")
        .compile()
        .expect("无法编译应用图标资源");
    // GNU linkers may discard a resource-only archive with no referenced symbols.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        let out = std::env::var("OUT_DIR").unwrap();
        println!("cargo:rustc-link-arg={out}/resource.o");
    }
}

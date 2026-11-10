fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_manifest_file("app.manifest")
            .set("ProductName", "FLA Reanim Compiler")
            .set("FileDescription", "Native Rust FLA / Reanim converter")
            .compile()
            .expect("embed Windows application manifest");
    }
}

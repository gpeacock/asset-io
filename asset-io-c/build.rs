fn main() {
    let root = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let bindings = cbindgen::Builder::new()
        .with_crate(&root)
        .with_config(cbindgen::Config::from_file(root.join("cbindgen.toml")).unwrap())
        .generate()
        .expect("generate asset_io.h");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("asset_io.h");
    bindings.write_to_file(&out);
    if std::env::var_os("ASSET_IO_UPDATE_HEADER").is_some() {
        bindings.write_to_file(root.join("include/asset_io.h"));
    } else {
        let checked = std::fs::read(root.join("include/asset_io.h"))
            .expect("missing header: build with ASSET_IO_UPDATE_HEADER=1");
        assert_eq!(
            checked,
            std::fs::read(out).unwrap(),
            "stale header: build with ASSET_IO_UPDATE_HEADER=1"
        );
    }
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-changed=include/asset_io.h");
    println!("cargo:rerun-if-env-changed=ASSET_IO_UPDATE_HEADER");
}

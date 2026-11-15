fn main() {
    let root = "../../vendor/anki/proto";
    println!("cargo:rerun-if-changed={root}");
    prost_build::Config::new()
        .file_descriptor_set_path(
            std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("scheduler.bin"),
        )
        .compile_protos(&[format!("{root}/anki/scheduler.proto")], &[root])
        .expect("compile scheduling descriptors");
}

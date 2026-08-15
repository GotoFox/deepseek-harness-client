fn main() {
    // Inject the dsh kernel version prepared by client/scripts/prepare.mjs so
    // the Rust shell knows which bundled kernel it shipped with.
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let version_file = std::path::Path::new(&manifest).join("../resources/kernel.version");
    let version = std::fs::read_to_string(version_file)
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=DSH_KERNEL_VERSION={version}");
    tauri_build::build()
}

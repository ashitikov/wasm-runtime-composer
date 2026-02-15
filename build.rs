use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let guests_dir = manifest_dir.join("tests/guests");

    // Use a separate target dir to avoid Cargo lock contention with the outer build
    let target_dir = guests_dir.join("target");

    for guest in &["producer", "consumer"] {
        let manifest = guests_dir.join(guest).join("Cargo.toml");

        println!("cargo::rerun-if-changed=tests/guests/{}/src/", guest);

        let status = Command::new("cargo")
            .arg("+nightly")
            .arg("build")
            .arg("--target")
            .arg("wasm32-wasip2")
            .arg("--release")
            .arg("--manifest-path")
            .arg(&manifest)
            .env("CARGO_TARGET_DIR", &target_dir)
            .status()
            .unwrap_or_else(|e| panic!("Failed to run cargo build for {}: {}", guest, e));

        assert!(
            status.success(),
            "Failed to build guest component '{}'",
            guest,
        );
    }

    println!("cargo::rerun-if-changed=tests/guests/wit/");
}

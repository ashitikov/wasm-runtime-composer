use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Test guests are needed only when composer is the primary package
    // being built (its own `cargo test` runs). When composer is consumed
    // as a path/git dependency, downstream consumers must not pay the
    // wasm32-wasip2 toolchain cost, and crucially must not fail when
    // their pinned wasmtime / wit-bindgen versions disagree with the
    // ones the test guests were authored against. Opt-in env override
    // for explicit local guest rebuilds when working on composer tests
    // from a downstream workspace.
    let is_primary = std::env::var("CARGO_PRIMARY_PACKAGE").is_ok();
    let force = std::env::var("BUILD_COMPOSER_TEST_GUESTS").is_ok();
    if !is_primary && !force {
        return;
    }

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
            .env_remove("RUSTUP_TOOLCHAIN")
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

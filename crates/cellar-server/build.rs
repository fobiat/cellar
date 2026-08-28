use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let commit = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=CELLAR_BUILD_COMMIT={commit}");
}

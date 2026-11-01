//! Stamps the build with the commit it came from, so `seavx update` can tell two
//! builds of the same version apart and `--version` can name the commit.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=SEAVX_GIT_SHA");

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let sha = std::env::var("SEAVX_GIT_SHA")
        .ok()
        .filter(|sha| !sha.trim().is_empty())
        .or_else(git_short_sha);

    match sha {
        Some(sha) => {
            let sha = sha.trim();
            println!("cargo:rustc-env=SEAVX_GIT_SHA={sha}");
            println!("cargo:rustc-env=SEAVX_VERSION={version} ({sha})");
        }
        // No checkout to ask (a release tarball, say): the version stands alone
        // and `update` compares versions only.
        None => println!("cargo:rustc-env=SEAVX_VERSION={version}"),
    }
}

fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#![forbid(unsafe_code)]

use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=MIRAGE_BUILD_HASH");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    let rustc = env::var_os("RUSTC")
        .and_then(|path| Command::new(path).arg("--version").output().ok())
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|version| version.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let build_hash = env::var("MIRAGE_BUILD_HASH")
        .ok()
        .filter(|value| valid_build_hash(value))
        .or_else(git_build_hash)
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=MIRAGE_RUSTC_VERSION={rustc}");
    println!("cargo:rustc-env=MIRAGE_TARGET={target}");
    println!("cargo:rustc-env=MIRAGE_BUILD_HASH={build_hash}");
}

fn git_build_hash() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim().to_string();
    valid_build_hash(&value).then_some(value)
}

fn valid_build_hash(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

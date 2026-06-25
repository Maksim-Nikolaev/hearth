//! Embed the git commit the binary was built from, so a stale build (or a
//! Linux↔Windows version mismatch) is obvious at startup and on the login
//! screen via `env!("HEARTH_GIT_SHA")`.

use std::process::Command;

fn main() {
    let sha = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let label = if dirty { format!("{sha}-dirty") } else { sha };

    println!("cargo:rustc-env=HEARTH_GIT_SHA={label}");

    // Refresh the SHA when the checked-out commit or working tree changes.
    for p in ["../.git/HEAD", "../.git/index"] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

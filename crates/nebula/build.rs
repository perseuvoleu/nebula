//! Stamp the binary with the git commit it was built from, so `nebula
//! --version` tells two machines' builds apart even when the crate version
//! matches. `NEBULA_BUILD_ID` set at build time wins (a build without .git).

use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=NEBULA_BUILD_ID");
    if let Ok(id) = std::env::var("NEBULA_BUILD_ID") {
        println!("cargo:rustc-env=NEBULA_BUILD_ID={id}");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Only what goes into the binary decides "-dirty": an edited doc or
    // web file doesn't make the build differ from its commit.
    let sources = ["crates", "vendor", "Cargo.toml", "Cargo.lock"];
    for source in sources {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
    }
    for git_path in ["HEAD", "index", "packed-refs"] {
        if let Some(p) = git(
            &root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                git_path,
            ],
        ) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    if let Some(head_ref) = git(&root, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(p) = git(
            &root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &head_ref,
            ],
        ) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    let id = match git(&root, &["rev-parse", "--short=7", "HEAD"]) {
        Some(sha) => {
            let mut status = vec!["status", "--porcelain", "--"];
            status.extend(sources);
            let dirty = git(&root, &status).is_some_and(|s| !s.is_empty());
            if dirty {
                format!("{sha}-dirty")
            } else {
                sha
            }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=NEBULA_BUILD_ID={id}");
}

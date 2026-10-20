//! The git commit this binary was built from (`<sha>` or `<sha>-dirty`),
//! stamped by the `nebula` crate's build script. Library crates read it
//! here; tests that never run the binary's `main` see "unknown".

use std::sync::OnceLock;

static BUILD_ID: OnceLock<&'static str> = OnceLock::new();

pub fn set(id: &'static str) {
    let _ = BUILD_ID.set(id);
}

pub fn get() -> &'static str {
    BUILD_ID.get().copied().unwrap_or("unknown")
}

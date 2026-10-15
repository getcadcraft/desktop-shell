// SPDX-License-Identifier: Apache-2.0
fn main() {
    // TB.3: the iPad half of the shell's own mobile plugin (ios/plugin, a Swift package, src/mobile.rs) is linked into the
    // app, as Tauri's own plugins link theirs. Only when building for iOS, which happens on a Mac (IPAD.md).
    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        ios_plugin();
    }
    tauri_build::build()
}

#[cfg(target_os = "macos")]
fn ios_plugin() {
    use std::path::{Path, PathBuf};
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let plugin = manifest.join("../ios/plugin");
    println!("cargo:rerun-if-changed={}", plugin.display());
    // The Tauri Swift API the package depends on (../.tauri/tauri-api), from the tauri crate.
    let api = PathBuf::from(std::env::var("DEP_TAURI_IOS_LIBRARY_PATH").expect("DEP_TAURI_IOS_LIBRARY_PATH: tauri must be a dependency"));
    let dest = manifest.join("../ios/.tauri/tauri-api");
    let _ = std::fs::remove_dir_all(&dest);
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let name = e.file_name();
            if [".build", "Package.resolved", "Tests"].iter().any(|x| name == *x) {
                continue;
            }
            let (f, t) = (e.path(), to.join(&name));
            if f.is_dir() { copy(&f, &t) } else { std::fs::copy(&f, &t).unwrap(); }
        }
    }
    copy(&api, &dest);
    tauri_utils::build::link_apple_library("cadcraft-plugin", &plugin);
}

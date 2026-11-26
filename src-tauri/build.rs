use std::ffi::OsStr;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=Info.plist");
    tauri_build::build();

    // During `tauri dev` on macOS, the app runs as a bare executable rather than a `.app` bundle.
    // Tauri embeds `Info.plist` into the binary for in-process AppKit, but macOS's out-of-process
    // file picker (`openAndSavePanelService`) reads `Info.plist` from disk beside the executable.
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(OsStr::new("macos")) && tauri_build::is_dev() {
        let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
        if let Some(target_dir) = out_dir
            .ancestors()
            .find(|path| path.file_name() == Some(OsStr::new("build")))
            .and_then(|build| build.parent())
        {
            let source =
                PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"))
                    .join("Info.plist");
            if source.is_file() {
                std::fs::copy(&source, target_dir.join("Info.plist"))
                    .expect("failed to copy Info.plist beside dev binary");
            }
        }
    }
}

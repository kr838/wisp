fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let native = manifest.parent().unwrap().join("native");
    // The DLL is loaded at RUNTIME (see selection.rs), so it is not linked. Nothing to add to
    // the link line — which is also why test binaries no longer need a copy of it next to
    // themselves (`STATUS_ENTRYPOINT_NOT_FOUND` used to abort `cargo test --lib`).
    //
    // The GNU toolchain still links a console-subsystem binary by default, which makes a
    // black terminal window appear when the app launches. Force the GUI subsystem.
    println!("cargo:rustc-link-arg=-Wl,--subsystem,windows");

    // Also make the DLL discoverable for `cargo run` / tests started from the crate root.
    copy_native_dll(native.join("SelectionHook.dll"));
    println!("cargo:rerun-if-changed={}", native.join("SelectionHook.dll").display());

    // The installer bundles `src-tauri/SelectionHook.dll` (see tauri.conf.json's `resources`),
    // which is a committed copy of the built DLL. If it drifts from `native/`, the packaged app
    // silently ships an old engine — e.g. one without `sh_request_selection`, which would make
    // the tray "show toolbar" entry stop working while every local build looked fine.
    sync_bundled_dll(&native.join("SelectionHook.dll"), &manifest.join("SelectionHook.dll"));

    tauri_build::build()
}

/// Keep the committed copy used by the installer in step with the freshly built DLL.
///
/// A build script cannot write outside OUT_DIR in a way cargo tracks, so this copies the file
/// instead of failing the build: the warning makes a stale bundle visible without blocking
/// `cargo check` in a checkout that has not built the native side yet.
fn sync_bundled_dll(native_dll: &std::path::Path, bundled: &std::path::Path) {
    if !native_dll.exists() {
        println!("cargo:warning=native/SelectionHook.dll is missing; the installer will contain no engine");
        return;
    }
    let same = std::fs::metadata(bundled)
        .and_then(|m| m.modified())
        .ok()
        .zip(std::fs::metadata(native_dll).and_then(|m| m.modified()).ok())
        .map(|(bundled_t, native_t)| bundled_t >= native_t)
        .unwrap_or(false);
    if same {
        return;
    }
    match std::fs::copy(native_dll, bundled) {
        Ok(_) => println!(
            "cargo:warning=refreshed src-tauri/SelectionHook.dll from native/ (it was older than the built engine)"
        ),
        Err(e) => println!("cargo:warning=could not refresh src-tauri/SelectionHook.dll: {e}"),
    }
}

/// Put the runtime DLLs next to the build outputs.
///
/// Two different lookups are involved:
/// * `SelectionHook.dll` — the app loads it itself (see `selection.rs`), from beside the exe or
///   from the working directory / `native/`.
/// * `WebView2Loader.dll` — imported by the Tauri runtime. A test binary lives in
///   `target/<profile>/deps/`, so a copy only in `<profile>/` is not found and the process
///   aborts before running any test.
///
/// Copying both into the profile folder and `deps/` makes `cargo test` and `cargo run` work
/// from a clean checkout; packaged builds get them through the bundle resources.
fn copy_native_dll(src: std::path::PathBuf) {
    if !src.exists() {
        return;
    }
    // OUT_DIR = <target>/<profile>/build/<crate>-<hash>/out
    let out_dir = match std::env::var_os("OUT_DIR") {
        Some(v) => std::path::PathBuf::from(v),
        None => return,
    };
    let Some(profile_dir) = out_dir.ancestors().nth(3).map(|p| p.to_path_buf()) else {
        return;
    };

    let mut files = vec![src];
    // WebView2Loader ships with the webview2-com-sys build script output.
    if let Ok(entries) = std::fs::read_dir(out_dir.join("..").join("..")) {
        for e in entries.flatten() {
            let p = e.path();
            if p.file_name().and_then(|n| n.to_str()).map(|n| n.starts_with("webview2-com-sys")).unwrap_or(false) {
                let dll = p.join("out").join("x64").join("WebView2Loader.dll");
                if dll.exists() {
                    files.push(dll);
                    break;
                }
            }
        }
    }

    for dir in [profile_dir.clone(), profile_dir.join("deps")] {
        if !dir.is_dir() {
            continue;
        }
        for f in &files {
            if let Some(name) = f.file_name() {
                let target = dir.join(name);
                // Skip when it is already there and newer, to avoid touching a loaded DLL.
                let fresh = std::fs::metadata(&target)
                    .and_then(|m| m.modified())
                    .ok()
                    .zip(std::fs::metadata(f).and_then(|m| m.modified()).ok())
                    .map(|(t, s)| t >= s)
                    .unwrap_or(false);
                if !fresh {
                    // A concurrent build (or a still-loaded DLL) can hold the target file; a
                    // failed copy is harmless at build time and must not fail the build.
                    let _ = std::fs::copy(f, target);
                }
            }
        }
    }
}

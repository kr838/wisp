fn main() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let native = manifest.parent().unwrap().join("native");
    println!("cargo:rustc-link-search=native={}", native.display());
    println!("cargo:rustc-link-lib=dylib=SelectionHook");
    // The import library embeds a static-libgcc unwinder that collides with the one
    // Rust links; both implementations are equivalent so the duplicate is harmless.
    println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
    // The GNU toolchain links a console-subsystem binary by default, which makes a
    // black terminal window appear when the app launches. Force the GUI subsystem so
    // it runs as a normal windowed application.
    println!("cargo:rustc-link-arg=-Wl,--subsystem,windows");

    // Integration tests link the library, so they need the same linker relaxation.
    // `rustc-link-arg` above applies only to the crate's own lib/bin targets; without
    // these, test binaries fail at load time with STATUS_ENTRYPOINT_NOT_FOUND.
    println!("cargo:rustc-link-arg-tests=-Wl,--allow-multiple-definition");

    tauri_build::build()
}

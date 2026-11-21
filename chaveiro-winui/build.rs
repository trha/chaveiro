//! Generates the HTTP and Windows Hello bindings into `OUT_DIR/bindings.rs` from the
//! Windows metadata that windows-bindgen ships, with the members
//! `bindings.txt` names.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=bindings.txt");

    // windows-bindgen runs `rustfmt` by name. It sits next to the `cargo` that
    // runs this script, which need not be on PATH.
    if let Some(cargo_directory) = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .and_then(|cargo| cargo.parent().map(PathBuf::from))
    {
        let path = std::env::var_os("PATH").unwrap_or_default();
        // An entry that cannot be joined back (one holding a `;`) leaves PATH
        // as it is; rustfmt may then still be found there.
        if let Ok(joined) = std::env::join_paths(
            std::iter::once(cargo_directory).chain(std::env::split_paths(&path)),
        ) {
            // SAFETY: the build script is single-threaded at this point.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }

    let out =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("bindings.rs");
    let filter = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("bindings.txt");
    // windows-bindgen takes `&str` arguments.
    let text = |path: &PathBuf| {
        path.to_str()
            .unwrap_or_else(|| panic!("windows-bindgen needs UTF-8 paths: {}", path.display()))
            .to_owned()
    };
    windows_bindgen::bindgen([
        "--out",
        &text(&out),
        "--flat",
        "--dead-code",
        "--minimal",
        "--filter-file",
        &text(&filter),
    ]);
}

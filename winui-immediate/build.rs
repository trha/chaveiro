//! Generates the WinUI bindings into `OUT_DIR/bindings.rs` from the Windows
//! App Runtime metadata, with the members `bindings.txt` names.
//!
//! The metadata comes from the installed runtime package, or from the folder
//! in `WINUI_IMMEDIATE_WINMD`.

use std::path::PathBuf;

fn main() {
    // The bindings are Windows only, and the crate is empty on other targets.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    println!("cargo:rerun-if-changed=bindings.txt");
    println!("cargo:rerun-if-env-changed=WINUI_IMMEDIATE_WINMD");

    let winmd = std::env::var_os("WINUI_IMMEDIATE_WINMD")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files\WindowsApps").join(RUNTIME_PACKAGE));
    assert!(
        winmd.is_dir(),
        "no Windows App Runtime metadata at {}: install the runtime or set WINUI_IMMEDIATE_WINMD",
        winmd.display()
    );

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
        "--in",
        "default",
        &text(&winmd),
        "--out",
        &text(&out),
        "--flat",
        "--dead-code",
        "--minimal",
        "--filter-file",
        &text(&filter),
        "--implement",
        "Microsoft.UI.Xaml.IApplicationOverrides",
        "Microsoft.UI.Xaml.Markup.IXamlMetadataProvider",
        "Microsoft.UI.Xaml.IElementFactory",
        "--compose",
        "Microsoft.UI.Xaml.Application",
    ]);
}

/// The runtime whose metadata the bindings target. `startup.rs` asks for this
/// version or newer at run time.
const RUNTIME_PACKAGE: &str = "Microsoft.WindowsAppRuntime.2_2.4.0.0_x64__8wekyb3d8bbwe";

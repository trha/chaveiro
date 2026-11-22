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
    if std::env::var_os("CARGO_FEATURE_COUNT_CALLS").is_some() {
        let generated = std::fs::read_to_string(&out).expect("windows-bindgen wrote the bindings");
        std::fs::write(&out, count_clones(&count_calls(&generated))).expect("OUT_DIR is writable");
    }
}

/// `bindings` with a call to `crate::calls::count("Type::Method")` at the
/// top of every generated method that takes `&self`, which is every call
/// into a WinUI object.
fn count_calls(bindings: &str) -> String {
    let mut counted = String::with_capacity(bindings.len() * 2);
    let mut owner = "";
    let mut rest = bindings;
    while let Some(start) = rest.find("fn ") {
        let (before, function) = rest.split_at(start);
        counted.push_str(before);
        // The innermost `impl` before this function names its type.
        if let Some(line) = before.lines().rev().find(|line| line.starts_with("impl ")) {
            owner = line["impl ".len()..]
                .split(['<', ' ', '{'])
                .next()
                .unwrap_or_default();
        }
        let name: &str = &function["fn ".len()..][..function["fn ".len()..]
            .find(|character: char| !(character.is_alphanumeric() || character == '_'))
            .unwrap_or(0)];
        let body = function.find('{');
        let declaration_end = function.find(';');
        let public = before.ends_with("pub(crate) ") || before.ends_with("pub ");
        match body {
            Some(body)
                if public
                    && declaration_end.is_none_or(|end| body < end)
                    && function[..body].contains("&self") =>
            {
                counted.push_str(&function[..=body]);
                counted.push_str(&format!("crate::calls::count(\"{owner}::{name}\");"));
                rest = &function[body + 1..];
            }
            _ => {
                counted.push_str("fn ");
                rest = &function["fn ".len()..];
            }
        }
    }
    counted.push_str(rest);
    counted
}

/// `bindings` with each runtime class's derived `Clone` replaced by one that
/// calls `crate::calls::count("Type::clone")`, because a clone is a COM
/// `AddRef`, and its drop a `Release`, which no method count sees. Clones
/// of the interfaces that windows-core's macro defines stay uncounted.
fn count_clones(bindings: &str) -> String {
    const DERIVE: &str = "#[derive(Clone, Debug, Eq, PartialEq)]";
    const FIELD: &str = "(windows_core::IUnknown);";
    let mut counted = String::with_capacity(bindings.len() + bindings.len() / 8);
    let mut lines = bindings.lines().peekable();
    while let Some(line) = lines.next() {
        let class = lines
            .peek()
            .and_then(|next| next.strip_prefix("pub struct "))
            .and_then(|next| next.strip_suffix(FIELD));
        match class {
            Some(class) if line == DERIVE => {
                counted.push_str("#[derive(Debug, Eq, PartialEq)]\n");
                counted.push_str(&format!(
                    "pub struct {class}{FIELD}\n\
                     impl Clone for {class} {{ fn clone(&self) -> Self {{ \
                     crate::calls::count(\"{class}::clone\"); Self(self.0.clone()) }} }}\n"
                ));
                lines.next();
            }
            _ => {
                counted.push_str(line);
                counted.push('\n');
            }
        }
    }
    counted
}

/// The runtime whose metadata the bindings target. `startup.rs` asks for this
/// version or newer at run time.
const RUNTIME_PACKAGE: &str = "Microsoft.WindowsAppRuntime.2_2.4.0.0_x64__8wekyb3d8bbwe";

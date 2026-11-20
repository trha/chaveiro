//! Test 16: `vault-core` stays platform-neutral and free of I/O crates.

use std::process::Command;

#[test]
fn no_platform_http_async_or_file_crates() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "vault-core",
            "-e",
            "normal",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).expect("cargo tree prints UTF-8");
    let banned: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| {
            BANNED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
        .collect();
    assert!(banned.is_empty(), "banned crates: {banned:?}");
}

/// Platform bindings, HTTP clients, async runtimes and file helpers.
const BANNED_PREFIXES: &[&str] = &[
    "windows",
    "objc2",
    "core-foundation",
    "http",
    "hyper",
    "reqwest",
    "ureq",
    "tokio",
    "async-",
    "futures",
    "mio",
    "tempfile",
    "getrandom",
];

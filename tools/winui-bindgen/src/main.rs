//! Generates minimal WinUI bindings from the Windows App SDK metadata.
//!
//! Usage: `winui-bindgen <winmd folder> <output .rs> <filter file>`
//!
//! Filter: `spikes.txt` for `winui-spikes/src/bin/immediate`. The `winui-immediate`
//! crate generates its own bindings in `build.rs`.

fn main() {
    let mut args = std::env::args().skip(1);
    let winmd = args.next().expect("winmd folder");
    let out = args.next().expect("output path");
    let filter = args.next().expect("filter file");
    windows_bindgen::bindgen([
        "--in",
        "default",
        &winmd,
        "--out",
        &out,
        "--flat",
        "--dead-code",
        "--minimal",
        "--filter-file",
        &filter,
        "--implement",
        "Microsoft.UI.Xaml.IApplicationOverrides",
        "Microsoft.UI.Xaml.Markup.IXamlMetadataProvider",
        "Microsoft.UI.Xaml.IElementFactory",
        "Windows.Graphics.Effects.IGraphicsEffect",
        "Windows.Graphics.Effects.IGraphicsEffectSource",
        "--compose",
        "Microsoft.UI.Xaml.Application",
    ]);
}

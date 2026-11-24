---
name: winui-startup-cost
description: measured chaveiro startup to first frame (~215 ms release) and which workarounds failed
metadata:
  type: project
---

Measured 2026-11-24, release build, process creation to `Window.Activate`.
Total was ~245 ms. It is ~215 ms after `compact()` switched to one Win32
style write. A plain Win32 window takes ~100 ms. The release binary is
`target/release/chaveiro.exe`.

Fixed WinUI cost: bootstrap 4 ms, `Application::Start` to callback 25 ms,
`OnLaunched` + `XamlControlsResources` 12 ms, `Window::new` 31 ms,
`SetExtendsContentIntoTitleBar` ~55 ms, first layout ~35 ms. About 1 run in
4 spikes by 150–450 ms in `Window::new` or before it.

Workarounds that failed:
- Extending through `AppWindow.TitleBar` costs the same 50 ms.
- Extending right after `Window::new` costs 80 ms.
- Fitting on the first pass measures too small (360x250 against 429.6),
  because controls apply their templates only once they are in the live
  tree, and `XamlRoot` is null then. You see the resize.
- Revealing on render tick 1 saves only ~4 ms.

How to measure: buffer the marks in memory and print them at the end,
because one `eprintln!` to redirected stderr cost ~11 ms.

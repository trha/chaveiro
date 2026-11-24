---
name: winui-live-theme
description: Measured WinUI 3 (WinAppSDK 2.7) facts for following a live system theme change - theme brushes from code, ThemeDictionaries trap, caption buttons, DWM Mica
metadata:
  type: project
---

Measured 2026-11-23 by flipping `AppsUseLightTheme` while the examples ran
(script: set it, broadcast `WM_SETTINGCHANGE` "ImmersiveColorSet", screenshot,
restore).

- A brush looked up in code (`Application.Resources` lookup) is the current
  theme's brush and never updates. `{ThemeResource}` has no stable code API:
  `FrameworkElement.SetThemeResourceBinding` exists only behind
  `Feature_ExperimentalApi` (added 2026-07-25), and is missing from the 2.7.0
  winmd. So `winui-immediate/src/theme.rs` keeps every brush set through
  `set_brush` / `style_resources` and looks it up again on the element's
  `ActualThemeChanged`. New theme brushes must go through it, never
  `resource::<Brush>` directly.
- Reading `XamlControlsResources.ThemeDictionaries` (to look up another
  theme's brush) stops the whole app from following the system theme.
  Never touch it.
- An `ElementTheme` lookup through the app resources after the change gives
  the new theme's brush, so a re-lookup on `ActualThemeChanged` is enough.
  A subtree with its own `RequestedTheme` still gets the app theme's brushes.
- A shared `ResourceDictionary` cannot be merged into two elements
  ("Element is already the child of another element").
- Caption buttons of a window that extends into the title bar keep their
  start theme on a live change, also before any of our changes (old build
  measured). Setting `AppWindowTitleBar.PreferredTheme` (on
  `IAppWindowTitleBar3`) each change fixes it; `follow_theme` in
  `widget/window.rs` does this and sets `DWMWA_USE_IMMERSIVE_DARK_MODE`.
- DWM Mica (`DWMWA_SYSTEMBACKDROP_TYPE` + `DwmExtendFrameIntoClientArea(-1)`
  + a clear composition brush as the window's backdrop) resizes with the
  frame; WinUI's `MicaBackdrop` lags and shows black edges on a drag.

**Why:** the user saw stale card text, Mica and caption buttons after a
system theme switch.

**How to apply:** use `theme::set_brush` for any theme brush in a widget,
and rerun the flip script after touching theme code. Capture the window
with `PrintWindow(hwnd, dc, 2)`: a screen copy shows whatever is on top, and
`SetForegroundWindow` is blocked while the editor has focus.

---
name: morph-blur
description: Measured facts behind Ui::morph: real Gaussian blur without Win2D, which ways of hiding the source work, the split composition bindings
metadata:
  type: project
---

Measured 2026-11-24 with `cargo run -p winui-spikes --bin morph` (frames captured with `PrintWindow(hwnd, dc, 2)`).

- A real Gaussian blur works without Win2D. A hand-written `IGraphicsEffect` + `IGraphicsEffectD2D1Interop` (declared with `interface_decl!`, `CLSID_D2D1GaussianBlur`, property `BlurAmount` mapped DIRECT to index 0) goes to `CreateEffectFactoryWithProperties(effect, ["Blur.BlurAmount"])`. Source: `CompositionVisualSurface` of `GetElementVisual(child)`, then a surface brush, then `SetSourceParameter("source", ...)`.
- A visual surface still draws its source when the source is hidden with XAML `UIElement.Opacity = 0`, and also when the handoff visual's opacity is 0. Hiding a wrapper `Border` around the source makes the surface empty.
- Why, from the WinUI source (`~/ws/microsoft-ui-XAML`): XAML `UIElement.Opacity` is written to the handoff visual (`HWCompNodeWinRT.cpp`: `GetHandOffVisual` returns `m_primaryVisual`, and the static opacity goes to `m_primaryVisual->put_Opacity`), so both hiding methods set the same property. XAML's render walk (`hwwalk.cpp`, `skipRenderWhileTransparent`) skips the whole subtree of an opacity-0 element, unless that element itself has `IsUsingHandOffVisual()`, which `GetElementVisual` turns on. The flag is per element, so an opacity-0 parent `Border` is still skipped, and the child's visual inside it gets no content. That the surface ignores its source root's own opacity happens in the compositor, which this repo does not hold.
- The surface ignores layout. Set `SourceOffset = -pad` and `SourceSize = size + 2*pad` (about 3σ) by hand, and put the sprite at `ActualOffset - pad`.
- `ScopedBatch.Completed` returns an `EventRevoker`. Replacing it revokes the old handler, so a superseded morph never runs its finish step. Drop it before you restart an animation, because a restart completes the old batch.
- In flat mode, `Microsoft.UI.Composition.Compositor` and the `Windows.UI.Composition.Compositor` already used in `bindings.txt` clash. The fix is a second filter, `winui-immediate/composition.txt`, generated to `bindings::composition`.

**Why:** the user asked for copy buttons that morph to a check mark with a real blur.
**How to apply:** reuse `Ui::morph` for any A↔B swap. Do not morph secrets, because the surface copy stays drawn until the morph ends (see [[threat-model]]).

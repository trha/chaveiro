---
name: theme-transitions
description: Measured how WinUI theme transitions behave when a winui-immediate pass swaps keyed children (winui-spikes/src/bin/transition)
metadata:
  type: project
---

Measured 2026-11-21 with `cargo run -p winui-spikes --bin transition`, by screen capture every ~20 ms.

- `AddDeleteThemeTransition` on a stack, child keyed by screen: plays, but in sequence. The old child stays fully drawn ~300 ms, fades out ~150 ms, then the new child fades in, ~700 ms in all. `VisualTreeHelper::GetChildrenCount` stays 1 the whole time, so the fading child is invisible to tree walks.
- Same transition, child under one fixed id: the text changes at once, so nothing animates. The diff must give a new identity per screen.
- `ContentPresenter` + `ContentThemeTransition`, `Content` swapped: old fades out and new slides in, ~200 ms. Best fit for screen changes.
- `EntranceThemeTransition` on a keyed child: new child slides in, old one vanishes at once.
- Push (column 5, added 2026-11-21): a clipped `Grid` holds both screens. Each gets `UIElement.TranslationTransition = Vector3Transition` (250 ms). The new screen gets `Translation = ±width` before `Append`, then `0` in the same pass; the old one gets `∓width`. Both screens move together in both directions, with no lag and no jump. The default easing is front-loaded: most of the motion shows in the first ~100 ms. The pass removes the old child after 350 ms. No Composition API is needed.

**Why:** the lock threat model needs to know that a removed element can stay drawn after the pass drops it.
**How to apply:** use the push for the login flow and a presenter with `ContentThemeTransition` for a fade; on lock, swap without a transition or clear `ContentTransitions` first. See [[threat-model]].

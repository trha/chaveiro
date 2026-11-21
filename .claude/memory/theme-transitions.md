---
name: theme-transitions
description: Measured how WinUI theme transitions behave when a winui-immediate pass swaps keyed children (spikes/src/bin/transition)
metadata:
  type: project
---

Measured 2026-11-21 with `cargo run -p spikes --bin transition`, by screen capture every ~20 ms.

- `AddDeleteThemeTransition` on a stack, child keyed by screen: plays, but in sequence. The old child stays fully drawn ~300 ms, fades out ~150 ms, then the new child fades in, ~700 ms in all. `VisualTreeHelper::GetChildrenCount` stays 1 the whole time, so the fading child is invisible to tree walks.
- Same transition, child under one fixed id: the text changes at once, so nothing animates. The diff must give a new identity per screen.
- `ContentPresenter` + `ContentThemeTransition`, `Content` swapped: old fades out and new slides in, ~200 ms. Best fit for screen changes.
- `EntranceThemeTransition` on a keyed child: new child slides in, old one vanishes at once.

**Why:** the lock threat model needs to know that a removed element can stay drawn after the pass drops it.
**How to apply:** use a presenter with `ContentThemeTransition` for navigation; on lock, swap without a transition or clear `ContentTransitions` first. See [[threat-model]].

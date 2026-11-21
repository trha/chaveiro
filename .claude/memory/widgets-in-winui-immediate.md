---
name: widgets-in-winui-immediate
description: New WinUI controls go into the winui-immediate crate as widgets; Ui::native is a last resort
metadata:
  type: feedback
---

When an app screen (chaveiro-winui) needs a WinUI control or property that winui-immediate lacks (ComboBox, InfoBar, ProgressRing, headers, IsEnabled, card styling, Enter key), add it to winui-immediate as a widget. Use `Ui::native` or casts on a `Response` only as a last resort.

**Why:** the user said so on 2026-11-21 after I built these as `native` helpers in chaveiro-winui/src/widget.rs. Other screens reuse them, and the crate keeps its write-on-change and shared-delegate model.

**How to apply:** draft the widget API first (the user likes API drafts before code), then implement in winui-immediate with its tests. See [[immediate-mode-spike]].

---
name: win32-resize-sync
description: Reverse-engineered Win32 mechanism behind UWP's frame/content synchronized resize (user32 ordinals, DComp sync object), and what WinUI 3 dropped
metadata:
  type: reference
---

Found 2026-11-24 from public PDBs (Windows 11 26100.8972) and disassembly.
Undocumented; ordinals may change between builds.

- user32 ordinal-only exports (forwarding to win32u `NtUser*`):
  - #2615 `EnableResizeLayoutSynchronization(HWND, BOOL)`: ApplicationFrame
    (`CApplicationFrame::RuntimeClassInitialize`) calls it once with TRUE.
  - #2614 `GetResizeDCompositionSynchronizationObject(HWND, HANDLE*)`:
    returns nonzero on success; ApplicationFrame calls it in
    `_NotifyPositionChange`, system XAML in
    `DXamlCore::SynchronizeCommitsWithWindow(HWND)`.
  - #2538 `LayoutCompleted(HWND)`, #2617 `IsResizeLayoutSynchronizationEnabled`.
  - Named `EnableSynchronizedLayout`.
- win32u `NtDCompositionCommitSynchronizationObject(HANDLE)` (also forwarded
  from dcomp.dll): system XAML's `WindowsPresentTarget::ProcessSynchronization`
  calls it from `DCompTreeHost::CommitMainDevice`, right after its DComp
  commit, when the stored `CommitSyncMode` is 3 (2 means waiting for layout).
- System XAML also uses public `Windows.UI.Core.CoreWindowResizeManager`
  (`CJupiterWindow::RegisterSynchronizedResize`) for CoreWindow.
- WinUI 3 keeps the private API `FrameworkApplication.SetSynchronizationWindow`
  but its body is `E_NOTIMPL`; the implementation never reached the repo.

**Spike result (2026-11-24): not usable by an ordinary app.**
- `EnableSynchronizedLayout(HWND, BOOL)` (named user32 export) sets the
  window's synchronized-layout flag, while `Feature_SynchronizedLayoutPublicApi`
  is on (else ERROR_NOT_SUPPORTED). Only then does #2614 return real handles,
  and only during a user drag; without it the handle is null.
- `NtDCompositionCommitSynchronizationObject` (win32kbase) returns
  STATUS_ACCESS_DENIED unless the caller is DWM or passes
  `RtlCapabilityCheck` for a "shellExperience..." capability. Measured: every
  commit from a WinUI 3 app failed with 0xC0000022, and the drag looked the
  same as without sync.
- The feature flag and a newer `NtUserCreateLayoutSyncForHwnd` (user32
  export) suggest a public API is coming; recheck on new Windows builds.
  The spike code is in the scratchpad as `tools\resize_sync_spike.rs`.

Tools and PDBs: scratchpad `tools\` (getpdb, publics, disasm, callers,
ordinals scripts; llvm-objdump from rustup `llvm-tools`). See
[[winui-live-theme]] for the Mica side of resizing.

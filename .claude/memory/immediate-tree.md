---
name: immediate-tree
description: The shared core crate under winui-, appkit- and dom-immediate; how the mac copy is edited from Windows (scp fails, tar over ssh works, fish shell)
metadata:
  type: project
---

`immediate-tree` holds the tree, frame stack, cache, state, task, stream and
flow for every `*-immediate` crate (spiked 2026-11-22, see
`plans/immediate-tree.md`). Each platform implements `Shell` with a public
newtype `NativeNode(pub(crate) Payload)`, because a crate-private type in the
public `impl Shell for Ui` is error E0446.

The appkit port lives only in the mac checkout (rmac `~/ws/chaveiro`); the
Windows checkout's `appkit-immediate` is older than the mac's. The two
checkouts are separate jj repos, so `immediate-tree` must be kept the same in
both by copying.

Editing the mac from Windows:
- `ssh -i C:/Users/tr/.ssh/rmac rmac '<cmd>'`; the remote shell is fish
  (`; and`, no `$(...)`), so put anything complex in a `bash` script.
- `scp` fails ("Connection closed"). Copy with tar over ssh through
  `cmd /c "... ssh ... \"tar cf - <dirs>\" > file.tar"` and the reverse with
  `type file.tar | ssh ... "tar xf -"`. PowerShell pipes mangle the bytes.
- System Events cannot see windows from the session, so `tools/smoke.sh`
  cannot run there (see [[appkit-immediate]]); starting an example for a few
  seconds still runs the idle pass check.

**Why:** the first port of appkit-immediate to the shared crate cost several
failed copy attempts before tar over ssh worked.

**How to apply:** for any change to `immediate-tree`, build and test both
checkouts, and copy the crate to the mac. See [[widgets-in-winui-immediate]].

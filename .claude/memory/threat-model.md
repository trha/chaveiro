---
name: threat-model
description: Agreed threat model for secrets in memory, and the chosen option C (no Reactor fork)
metadata:
  type: project
---

Agreed on 2026-11-20.

## Scope

- **In scope: secrets that outlive the unlocked session.** After lock or
  logout, our process must hold no master password, no derived keys and no
  decrypted vault data. This covers malware that arrives after lock, crash
  dumps (WER, debuggers, support tickets), the page file and `hiberfil.sys` on
  a stolen or imaged disk, and partly cold-boot or DMA reads of a locked,
  sleeping machine.
- **Best effort only: the unlocked state.** A process running as the same user
  can read our memory, log keystrokes or read the screen, so live copies while
  unlocked are not worth design contortions.
- **Out of scope:** admin, kernel or EDR-level attackers, and any same-user
  attacker while the vault is unlocked. Bitwarden's own clients make the same
  cut (bitwarden/clients#6231).

## Chosen option: C (no Reactor fork)

Rejected:
- **A**: fork Reactor for an uncontrolled `PasswordBox` read once on submit.
  It only removes Reactor's copies, which the wiping allocator already
  handles.
- **B**: fork Reactor and build our own input on `CoreTextEditContext` with a
  zeroing buffer. That is the only real `SecureString` path all the way
  through, but it means rebuilding caret, selection, IME, accessibility, paste
  and theming.

Option C is:
1. **Wiping global allocator.** Zero every block on free, and move by hand in
   `realloc`. Measured cost: none within noise (see
   [[windows-reactor-pitfalls]]).
2. **Clear on submit.** `mem::take` the password out of state, so the box
   resets to `""` and Reactor's live copies drop. Measured: 0 Rust copies
   after submit.
3. **Zeroizing secret types** (`zeroize` / `secrecy`) from our state into the
   SDK. Check what the Bitwarden SDK accepts.
4. **Clear everything on lock:** keys, decrypted items and form state.
5. **Accept WinUI's leftovers:** 1–2 UTF-16 HSTRING copies from
   `PasswordBox.Password` plus RichEdit's buffer
   (microsoft/microsoft-ui-xaml#11407). If they must not survive lock,
   restart the window or process on lock, as Bitwarden reloads its Electron
   processes. Untested.
6. **Reduce exposure:** Windows Hello unlock, so the master password is typed
   rarely; `VirtualLock` key material; keep our process out of crash dumps.

**Stack copies (measured in WP-06, 2026-11-20).** Moving a key struct
leaves copies in dead stack frames that no destructor zeroes. A scan after
lock found 2 copies of the user key on the UI thread's stack. `app` now
calls `wipe_stack` (192 KiB of zeros) after each input that leaves the vault
hidden, and the scan then found none. Scanner: ReadProcessMemory over
committed pages, searching each 32-byte half of the user key.

**Remembered second factor (2026-11-21).** The token from
`twoFactorRemember=1` lives in the protected `TwoFactorToken` slot and in
`Core`, and it survives lock and logout on purpose, as in Bitwarden's
clients. It only skips the second factor, so the master password is still
needed. The core sends it only for the server and email it was issued to
(the record names its server since 2026-11-21), and clears it when the
server refuses it.

WinRT strings bypass the wiping allocator. `take_password` in
winui-immediate zeroes the `HSTRING` that `PasswordBox.Password` returns and
hands back a `Zeroizing<String>`. The request body buffer in
chaveiro-winui's `http.rs` (which holds the password hash at login) is still
an unwiped WinRT copy.

**Why:** the user decided that memory hygiene matters only for what outlives
the unlocked session, since a live same-user attacker gets the password
anyway.

**How to apply:** judge every secret-handling change by "is it gone after
lock?". Do not add complexity to shave copies while unlocked. Revisit forks
A or B only if the threat model changes.

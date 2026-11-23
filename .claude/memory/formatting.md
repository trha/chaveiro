---
name: formatting
description: Format Rust with nightly rustfmt (cargo +nightly fmt); rustfmt.toml merges imports per crate (imports_granularity = "Crate"); one import block, no blank lines
metadata:
  type: feedback
---

Format every Rust change with nightly rustfmt:

    & "$env:USERPROFILE\.cargo\bin\cargo.exe" +nightly fmt -p <crate>

The root `rustfmt.toml` is the user's: `imports_granularity = "Crate"`, a
nightly-only option that merges imports into one `use` per crate
(`use std::{cell::RefCell, rc::Rc};`). Write imports as one block with no blank
lines between them; rustfmt keeps that layout. Do not edit `rustfmt.toml`
without asking; read it first, because the user changes it.

`~/.cargo/bin` is not on the agent shell's PATH, so call the rustup proxy by
its full path. Check that the run printed no "Could not parse TOML" error:
a broken config makes rustfmt skip formatting silently in a piped run.

**Why:** the user asked for nightly formatting and ungapped imports on
2026-11-20, and then for imports merged per crate.

**How to apply:** run it before every build check that closes a work step.
Format only the files you changed
(`& "$env:USERPROFILE\.cargo\bin\rustfmt.exe" +nightly --edition 2024 <file>...`), never the
whole workspace: much of the tree is unformatted, and the user wants that in
a separate format pass (asked 2026-11-23, after a workspace run touched 10
unrelated files). If a changed file has unformatted code of its own, keep
the hunks you did not write out of the diff.
See [[declaration-order]] and [[doc-links]] for the other style rules.

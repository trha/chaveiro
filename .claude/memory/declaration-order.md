---
name: declaration-order
description: Order declarations in a file top down: the high-level type or entry point first, its helpers and details after
metadata:
  type: feedback
---

Put the most important declaration first in each file: the main type and its
impl, or the entry function. Supporting types, constants, `link!` imports,
thread-locals and private helpers follow, in the order a reader needs them.
In test files, the tests come first and their helpers, strategies and test
allocators after.

A module doc stays short; a contract that ties a type's fields together goes
on that type, not on the module.

**Why:** the user asked for it on 2026-11-20 while reviewing `winui-immediate`
(`tree.rs` had a long module doc and the `Tree` type had none).

**How to apply:** when writing or editing a file, check the order before
finishing. `macro_rules!` definitions are the exception: they must come before
their invocations.

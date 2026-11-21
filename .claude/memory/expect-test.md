---
name: expect-test
description: expect_test pitfalls in vault-core tests: shared-helper snapshots and the empty literal
metadata:
  type: project
---

`vault-core/tests/core.rs` renders requests with `render(request, keep)`,
which redacts EncStrings, secrets, ids and dates. Snapshots live in
`expect![[...]]`.

- The helper `assert_logged_in` holds one snapshot that 4 tests reach.
  `UPDATE_EXPECT=1` with parallel tests writes that literal once per test
  and breaks the file, so update with `-- --test-threads=1`.
- In expect-test 1.5 an empty snapshot is `expect![[""]]`; `expect![[]]`
  panics.

**Why:** both cost a broken run on 2026-11-21.

**How to apply:** run updates single-threaded, then read every changed
snapshot in the diff before keeping it.

---
name: structured-concurrency
description: Strict structured concurrency; every thread, pool job and future has an owner that waits for it or aborts it
metadata:
  type: feedback
---

Use strict structured concurrency. Never let a thread or future run
unattended: every thread, pool job and spawned task has an owner that keeps
its handle and either waits for it or aborts it before the owner ends.

**Why:** the user asked for it on 2026-11-21, while we planned to move the
shell's work onto the OS thread pool and to chain storage jobs one after
another instead of keeping a storage thread.

**How to apply:**
- Keep each `winui_executor::SpawnHandle` in the state that owns the work;
  dropping it aborts. Treat `detach()` as a smell and ask before using it.
- Do not use bare `thread::spawn` or fire-and-forget pool submits. Keep the
  `JoinHandle` or an awaited future, so shutdown can wait or cancel.
- Code that predates this rule (`thread::spawn` for the Hello prompt in
  `app/src/shell.rs`, `.detach()` in `winui-executor/examples/check.rs`)
  needs rework when touched.

---
name: memory-location
description: Store all memories for this project in the project's .claude/memory folder
metadata:
  type: feedback
---

Write every memory for this project to `.claude/memory/` in the project root
(`C:\Users\tr\ws\bitwarden-winui\.claude\memory\`), with the index in
`.claude/memory/MEMORY.md`.

**Why:** the user asked on 2026-11-20 that all memories live in the project,
not in the user folder `C:\Users\tr\.claude`.

**How to apply:** do not write to `C:\Users\tr\.claude\projects\...\memory\`.
Read `.claude/memory/MEMORY.md` at the start of work on this project.

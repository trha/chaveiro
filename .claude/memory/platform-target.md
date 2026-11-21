---
name: platform-target
description: Sessions on Windows build chaveiro-winui against the "Chaveiro" design system artifact; the macOS canvas is for chaveiro-appkit
metadata:
  type: feedback
---

When the session runs on Windows, "the app" and "the artifact" mean
chaveiro-winui and the "Chaveiro" Design System artifact
(https://claude.ai/artifact/W6viMehdhBc5ftw9E4HT9p, screens under
`project/components/<Name>/`). The "Chaveiro macOS" canvas
(https://claude.ai/artifact/KWLJPX99DwD7nzxLasjLHQ) belongs to
chaveiro-appkit, which a session on a Mac builds.

**Why:** on 2026-11-21 I picked the macOS canvas and started installing an
Apple target on Windows; the user stopped it with "You're on windows".

**How to apply:** check the platform in the environment before picking a
crate or design. Do not add rustup targets to cross-check the other app.

---
name: core-errors-are-enums
description: chaveiro-core returns error enums, never user text; each UI crate writes and later localizes the sentences
metadata:
  type: feedback
---

Shared logic in chaveiro-core (such as `login_form`) returns enums like
`ServerError` and `EmailError`. It holds no user-facing strings, captions or
hints. Each UI crate (chaveiro-winui, chaveiro-appkit) maps them to its own text.

**Why:** the user wants localization to stay in the UI crates.

**How to apply:** when you move logic into chaveiro-core, turn each message
into a variant and put the `match` to text in the UI crate.

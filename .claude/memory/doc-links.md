---
name: doc-links
description: In doc comments, refer to items with rustdoc intra-doc links; keep parameters and non-items as plain code
metadata:
  type: feedback
---

When a doc comment names a type, function, method, field, variant or constant,
write it as an intra-doc link: [`Tree::mark_event`], [`close`](Self::close),
[`None`], [`u32::MAX`]. Parameters, file paths, environment variables, plan
names and items outside the crate graph (Win32 structs such as
`PACKAGE_VERSION`) stay as plain `code`. Do not link an item that does not
exist yet; add the link when the item lands.

Check with `cargo doc -p <crate> --no-deps --document-private-items`; it must
report no unresolved or ambiguous links.

**Why:** the user asked for it on 2026-11-20 while reviewing `winui-immediate`.

**How to apply:** every doc comment written or edited, together with
[[declaration-order]].

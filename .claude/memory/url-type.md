---
name: url-type
description: Use url::Url for every URL; never build or split URLs with format! or string slicing
metadata:
  type: feedback
---

Hold URLs as `url::Url`, and build them with `path_segments_mut().extend`,
`query_pairs_mut()` and `set_host`. Parse user input with `Url::parse` and
show the error. Form bodies use `url::form_urlencoded`.

**Why:** the user asked for a strong URL type and no hand-built URLs
(2026-11-20). Hand-built URLs miss percent-encoding and base-path cases.

**How to apply:** in `vault-core` the helper is `core::endpoint(base,
segments, query)`, and `ApiCall` holds path segments plus query pairs.
Comparing a URL to an expected string in a test is fine
(`url.as_str()`).

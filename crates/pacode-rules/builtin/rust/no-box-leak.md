---
name: rust-no-box-leak
languages: [rs]
match: '\bBox::leak\s*\('
scope: code
severity: warn
max_per_turn: 1
skip_test_code: true
---
`Box::leak` leaks memory for the rest of the process. Use an owned value, `Arc`, or a `OnceLock`/`LazyLock` static instead; if a leak is truly required, document why next to the call.

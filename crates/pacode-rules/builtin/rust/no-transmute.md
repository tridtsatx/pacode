---
name: rust-no-transmute
languages: [rs]
match: '\btransmute\s*(?:::\s*<[^>]*>\s*)?\('
scope: code
severity: warn
max_per_turn: 1
exclude: ['*_tests.rs', 'tests/']
---
Avoid `std::mem::transmute`. Use `from_ne_bytes`/`to_bits`/`TryFrom`/`bytemuck`-style safe conversions, a `#[repr]` enum with `TryFrom<u8>`, or a pointer cast. If it is unavoidable, add a `// SAFETY:` comment explaining layout and validity.

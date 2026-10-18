---
name: rust-no-lossy-cast
languages: [rs]
match: '\bas\s+(?:u8|u16|u32|i8|i16|i32|f32)\b'
scope: code
severity: warn
max_per_turn: 1
skip_test_code: true
exclude: ['*_tests.rs', 'tests/']
---
Low priority: `as` to a narrower numeric type silently truncates/wraps. Prefer `T::try_from(x)` (handle the error) or `.clamp(..)` / `saturating_*` first, and add a short comment if truncation is intended.

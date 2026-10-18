---
name: rust-unsafe-needs-safety
languages: [rs]
match: '\bunsafe\s*(?:\{|fn\b|impl\b|trait\b|extern\b)'
unless_above: '//\s*SAFETY:|///?\s*#\s*Safety|//!\s*#\s*Safety'
unless: '//\s*SAFETY:'
scope: code
severity: warn
max_per_turn: 2
exclude: ['*_tests.rs', 'tests/']
---
Every `unsafe` block/fn/impl needs a `// SAFETY:` comment on the line(s) directly above it, stating the invariant that makes it sound. Add the comment (or remove the `unsafe`).

---
name: ts-no-console-log
languages: [ts, tsx, mts, cts, js, jsx]
match: '\bconsole\.log\s*\('
scope: code
severity: warn
max_per_turn: 2
exclude: ['*.test.ts', '*.spec.ts', '*.test.tsx', '*.spec.tsx', '__tests__/', 'scripts/']
---
Remove the leftover `console.log`. Use the project's logger, or delete the debugging output.

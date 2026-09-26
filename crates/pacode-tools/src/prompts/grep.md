Search file contents with a regular expression (Rust regex syntax), respecting
.gitignore. Hits stop at `max_results`, so narrow broad searches with
`path`/`glob`. `glob` finds files by name; `symbols` maps functions/classes.

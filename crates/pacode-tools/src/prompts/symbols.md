Map a file's or directory's symbols — functions, methods, classes, impls — with
1-based line ranges. Use before reading a large file to see what lives where, or
across a tree to locate a definition `grep` would bury in call-site hits.
`mode: context` adds the first/last lines of each body. Files that fail to parse
or have no grammar are listed under `skipped`, never silently dropped. Not for
reading code (`read`), finding usages (`grep`), or finding files (`glob`).

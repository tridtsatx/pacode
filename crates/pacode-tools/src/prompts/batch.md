Parallel read-only lookups in one call: file reads, content search, file globs,
directory listings and read-only shell commands run concurrently and come back in
the order given. Prefer this over issuing the same calls one by one when
exploring. Writing is refused — editing tools and mutating shell commands never
run in a batch — and shell ops share a single permission prompt covering the
whole batch; refusing it fails the call. Every op's output is capped to a fair
share of the output budget, so for one big result call the tool directly instead.

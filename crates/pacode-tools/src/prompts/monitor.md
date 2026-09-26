Watch for a condition and be told once when it becomes true, instead of blocking
on a wait. `command_succeeds` = the command exits 0; `process_gone` = nothing
matches the `process` pattern any more; `file_matches` = `path` contains
`pattern`. A monitor fires at most once and stops with the session, but it polls
on its own, so `watch` and `stop` ask for permission.

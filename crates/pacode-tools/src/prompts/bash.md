Run a shell command in the working directory. A long command (build, test,
server) is moved to the background after a few seconds or on `background: true`:
you get a task id, keep working, and a notification arrives when it finishes. Use
`bg` to check, wait for, or kill tasks. Output above the cap is cut head+tail.

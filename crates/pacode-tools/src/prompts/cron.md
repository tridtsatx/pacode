Scheduled prompts. `add` a job that sends `prompt` to this session on a
`schedule`, `list` the jobs with the time until each next run, or `remove` one by
id. Jobs survive a daemon restart and fire without further approval, so `add` and
`remove` ask for permission. Use this for work that should happen again later,
not to wait for something to finish.

Load a skill's full instructions on demand: call it when one of the skills listed
in the system prompt matches the task. The body is capped (~16K) and a truncated
body ends with a `[... truncated ...]` marker.

Read a file. Returns lines prefixed with their 1-based number (`   12\tcode`).
Use offset/limit for large files; output above the cap is cut head+tail with a
marker reporting the dropped size. Binary files are refused.

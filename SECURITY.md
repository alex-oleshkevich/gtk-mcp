# Security policy

gtk-mcp is a development tool. The agent library runs inside your app's
process and accepts commands over a Unix socket in `$XDG_RUNTIME_DIR`, so
anything that can write to that socket can drive the app. Only add the agent
to development builds, and do not expose the socket to other users.

To report a vulnerability, use GitHub's private vulnerability reporting on
this repository (Security tab, "Report a vulnerability") instead of opening a
public issue.

# Herdr

[Herdr](https://herdr.dev) finds agents by their pane's foreground process, which in supervised
mode is nono rather than Claude, and its Claude integration resumes sessions with plain
`claude --resume`, outside the sandbox. When `cb` starts Claude Code in a Herdr pane
(`HERDR_ENV=1`), in either mode, cb's Claude Code plugin reports the agent itself.

Requires Herdr 0.9.2 or later and an agent with `kind = "claude"`. Outside Herdr, cb adds
nothing.

> [!WARNING]
> Reporting needs the sandbox to connect to the Herdr socket, and anything that can connect to it
> can control every Herdr pane, e.g. type into an unsandboxed shell. On Linux, nono's sandbox
> could already connect without this grant. Turn the integration off with
> `sandbox.nono.herdr = false`.

## State

Working, blocked on a permission prompt, or idle, from Claude Code's hooks (`cb hook herdr`).
Subagents' events are ignored.

## Resume

After a Herdr restart, the pane comes back through `cb ... -- --resume <session>`, keeping its
sandbox and cb profile.

- Kept: `-A`, `-p`, `--nono-profile`, the directory, sandbox arguments. `CLAUDE_CONFIG_DIR` and
  `CLANKERBOX_VAR_*` from your environment are passed along with `env`.
- Not kept: agent arguments after `--`, since a prompt among them would be sent again (put
  lasting ones in the agent's `args`), and `-y`, so new detected grants still need approval.
- A command Herdr wouldn't accept (an argument with `'`, over 64 arguments or 8 KiB) is left out;
  state is still reported.

## Herdr's own Claude hook

Once Herdr's hook reports a session, Herdr ignores cb's reports for the pane. So after you
approve a run inside Herdr, cb adds one line to that hook (`herdr-agent-state.sh`, found through
the hook commands in Claude's `settings.json`) to skip sessions cb reports.

- Other Claude sessions are unaffected, and the agent still sees `HERDR_*`.
- cb only edits Herdr's own copy: never through a symlink, and only a script with Herdr's Claude
  integration header; otherwise it warns.

`HERDR_AGENT` only helps Herdr's own process and screen detection, which can't see the agent in
supervised mode. You can leave it set; cb's reports don't need it.

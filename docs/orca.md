# Orca

To run Claude Code in [Orca](https://github.com/stablyai/orca) through cb, set Orca's command
override for Claude to `cb --`. Orca passes its own arguments after it, so resumed sessions run
through cb too.

In an Orca terminal, cb then makes these work without extra config:

- **Agent status.** Orca's status hooks run inside the sandbox. cb lets them read Orca's hook
  scripts (`~/.orca/agent-hooks`) and the file holding the port and token they report to. This
  applies to every agent.
- **Claude accounts.** Each run uses the account currently selected in Orca, even in a terminal
  opened before you switched. That account's config directory is granted read-write. If it is
  missing, cb refuses to run, as Orca does; `cb show` still works. A `CLAUDE_CONFIG_DIR` you set
  yourself still wins.

Outside Orca, cb adds nothing. Turn it off with `sandbox.orca = false`.

## `orca` CLI

Off by default. Turn it on with `sandbox.orca_cli = true` in the global config to let the agent
run `orca`, e.g. to create worktrees or read other terminals. Project files can't turn it on,
and `sandbox.orca = false` doesn't turn it off. cb then grants:

- Connect access to Orca's socket, and read access to the file naming it and holding its token.
- Read access to the `orca` launcher and Orca's install directory.

Anything that can connect to the socket can control every Orca terminal, including unsandboxed
shells. The socket changes when Orca restarts; restart the agent to reach the new one.

## Risks

- The hook scripts and endpoint are read-only, since unsandboxed Orca terminals run them too.
- The agent can read the endpoint's token, and so could send Orca false status updates.

## Limits

- **`network = "block"`:** cb opens the hook port Orca uses when the agent starts. After Orca
  restarts on a new port, status stops updating until you restart the agent.
- **Native chat mode:** if Orca starts Claude there through the override, there is no terminal
  for cb's approval prompt. Run `cb trust` in the project first.

# Sandbox

## What the agent can access

`cb show` lists every grant for a run and where it comes from.

Network access and environment variables follow the nono profile: by default, outbound network
is allowed and your environment is passed through.

### Always

- **The agent's nono profile:**
  - `claude` uses `cb:claude`, built into cb
    ([`src/sandbox/profiles/claude.json`](../src/sandbox/profiles/claude.json)) and adapted from
    nono's `nolabs-ai/claude` pack. cb creates the directories it grants before each run. Set
    `sandbox.nono.profile = "claude-code"` to use the pack instead.
  - `pi` and `omp` use the profiles of the same name, installed with `nono pull`.
  - `codex` and `opencode` use nono's built-in defaults.
- **Project root:** read-write, unless it is `$HOME` or contains it.
- **Current directory:** `--allow-cwd`, at the level the nono profile sets.
- **Linked git worktrees:** the shared git directory, and for `claude` the main worktree's
  `.claude/settings.local.json`.
- **Linux only:** the nono group `linux_temp_read`. In direct mode also read access to `/proc`,
  which exposes other processes' command lines: `nono wrap` grants `/proc/self` only to the
  first process, and child processes like Bun or browsers need it.

### Claude Code

- **Config directory:** read-write access to `$CLAUDE_CONFIG_DIR` and its `.lock` file when it
  is set (in `sandbox.env` or your environment).
- **Symlinked config:** read-only access to the targets of symlinks in the config directory and
  its `skills`, `agents`, `commands` and `output-styles` directories, e.g. a `settings.json` from
  your dotfiles. Grant `allow` on a target if Claude should be able to change it.
  - Targets that make up a whole directory are granted as that directory.
  - Symlinks are only followed in a config directory from your environment, the global config or
    the default `~/.claude`, never one a project file sets.
  - `$HOME` itself is never granted this way.
- **Denial hints:** cb loads a Claude Code plugin for the session (`--plugin-dir`, read-only in
  the sandbox). When a command fails with a permission error, its hooks ask Claude to confirm the
  cause with `nono why` and suggest the `cb` flag or config entry that would fix it. Your own
  plugins still load. The hooks run `cb hook explain-denial`, so the sandbox can also read the
  `cb` binary. Turn off with `sandbox.nono.explain_denials = false`.
- **Herdr socket:** inside a [Herdr](herdr.md) pane, connect access to `$HERDR_SOCKET_PATH`.
- **Orca:** inside an [Orca](orca.md) terminal:
  - Read access to Orca's hook scripts and endpoint, for any agent.
  - For `claude`, read-write access to the Claude account selected in Orca.
  - With `sandbox.orca_cli = true`, connect access to Orca's socket and read access to the CLI.

### Detected toolchains

cb looks for marker files such as `Cargo.toml` or `pom.xml` in the project root. In a git or jj
repository it also searches up to three directories below it, for monorepos; `--detect-subdirs`
does this outside a repository too. Dependency, build and hidden directories are skipped.

For each toolchain found, cb adds nono [groups](https://nono.sh/docs/cli/features/profiles-groups)
(e.g. `rust_runtime`) and write access to its shared package caches (e.g. `~/.cargo/registry`).

- `cb init` writes these into `.clankerbox.toml` for you to review and edit.
- Without a project config file, `cb` applies them directly and asks you to approve them (see
  [Trust](../README.md#trust)).

## How cb drives nono

- **[Execution mode](https://nono.sh/docs/cli/features/execution-modes):** `supervised`
  (default, `nono run`) or `direct` (`nono wrap`, for tools that need the agent as the foreground
  process).
- **Groups and built-in profiles:** nono has no flag for policy groups, so cb writes a small nono
  profile extending the base one to `$XDG_RUNTIME_DIR/clankerbox` (or `$XDG_CACHE_HOME/clankerbox`).
  nono can't extend a profile file, so for a built-in `cb:` profile or a profile file (a path,
  relative to the directory the agent runs in) cb writes a copy with the groups added instead.

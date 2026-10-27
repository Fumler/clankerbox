# clankerbox

`cb` runs AI coding agents (Claude Code, pi, codex, …) inside a [nono](https://nono.sh) sandbox,
with per-project rules. Plain `nono run` needs the right flags on every run; with `cb` a project
carries a `.clankerbox.toml` that says what the agent needs beyond the repo: a sibling repo to
read, a dependency checked out elsewhere, extra agent flags. Linux and macOS.

> [!NOTE]
> Largely written with AI (Claude Code) under human direction and review. Check `cb show` before
> trusting what a run grants.

## Quickstart

1. Install [nono](https://nono.sh), then the nono profile for your agent:

   ```sh
   nono pull nolabs-ai/claude      # or nolabs-ai/pi, nolabs-ai/omp
   ```

   If you skip this, `cb` offers to run it for you.

2. Install `cb` from a [release](https://github.com/Fumler/clankerbox/releases) or with Nix
   (`nix profile install github:Fumler/clankerbox`), then check the setup with `cb doctor`.

3. Run it. With no config at all, `cb` runs Claude Code sandboxed to the current project:

   ```sh
   cb                              # run claude in the sandbox
   cb --allow ./scratch            # one-off extra access
   cb show                         # what a run grants (paths it can access), and why
   ```

4. Optionally, add rules for a project that always needs extra access:

   ```sh
   cb init                         # write .clankerbox.toml, with groups for detected toolchains
   $EDITOR .clankerbox.toml        # e.g. read = ["../frontend"]
   cb                              # prompts once to trust the new file, then runs
   ```

Your own defaults (agents, cb profiles, extra access everywhere) go in an optional
[global config](#example-global-config).

## Usage

```
cb [OPTIONS | SANDBOX_ARGS]... [-- AGENT_ARGS...]
```

Recognised anywhere before a lone `--`:

| Option | |
|---|---|
| `-A/--agent <name>` | Run a different agent |
| `-p/--profile <name>` | Apply a [cb profile](#agents-and-cb-profiles) (repeatable) |
| `--nono-profile <name>` | Override the nono profile |
| `-C <dir>` | Run as if started in `<dir>` |
| `--no-project` | Ignore project config files |
| `--unsandboxed` | Run without a sandbox (or set `sandbox.backend = "none"`) |
| `--dry-run` | Print the command instead of running it |
| `--nono-<flag>` | Pass `--<flag>` to nono, for flags cb uses itself, e.g. `--nono-dry-run` |

Other arguments before `--` go to nono verbatim, e.g. `--allow ./dir`. Arguments after `--` go to
the agent.

| Subcommand | |
|---|---|
| `init [--local] [--force]` | Create `.clankerbox.toml`, or a personal `.clankerbox.local.toml` added to `.git/info/exclude` |
| `trust` / `untrust` | Approve or revoke the project config files that apply here |
| `show [--json] [ARGS]` | Config files, grants with their source, and the final command |
| `agents` | Available agents (`*` marks the default) and cb profiles |
| `edit [--local]` | Open the nearest config file in `$EDITOR`, then trust it |
| `doctor` | Check nono, the agent command, nono profiles and config files |
| `schema` | JSON Schema for config files |
| `completions <shell>` | Shell completions |

## Defaults

With no config, cb grants only:

- **Base policy:** the agent's nono profile: `claude-code` for `claude`, `pi` for `pi`, `omp` for
  `omp` (installed with `nono pull`). `codex` and `opencode` get nono's built-in defaults.
- **Project root:** read-write, unless it is `$HOME` or contains it.
- **Current directory:** `--allow-cwd`, at the level the nono profile sets.
- **Linked git worktrees:** the shared git directory, and for `claude` the main worktree's
  `.claude/settings.local.json`.

Network and environment variables follow the nono profile (by default: outbound network allowed,
environment passed through).

`cb init` adds only what detected toolchains need: nono
[groups](https://nono.sh/docs/cli/features/profiles-groups) (e.g. `rust_runtime` for a
`Cargo.toml`) and write access to their shared package caches (e.g. `~/.cargo/registry`), which
it warns about.

## Trust

A repository's config can grant access to anything, so project files must be approved, like
`direnv allow`. The first run with a new or changed file shows its contents and grants and asks
`Trust these files and continue? [y/N]`. Without a terminal, cb refuses until you run `cb trust`.
`init` and `edit` trust what they write; the global config is always trusted.

## Configuration

Layers, lowest precedence first:

1. Built-in presets: `claude` (the default), `codex`, `omp`, `opencode`, `pi` ([`src/presets.toml`](src/presets.toml))
2. Global config: `$XDG_CONFIG_HOME/clankerbox/config.toml`
3. Each `.clankerbox.toml` from the project root down to the current directory, each followed
   by its `.clankerbox.local.toml`
4. cb profiles selected with `-p`, in order
5. Command-line arguments

How layers combine:

- Lists are appended, single values replaced. Nothing can remove access.
- Unknown keys are errors.
- More specific settings win, lowest first: top-level `[sandbox]`, the agent it `inherits` from,
  the agent itself, then the same for each cb profile. Within each step, later files win.

Config files are searched from the current directory up to the git toplevel. Outside git, the
search stops below `$HOME`; outside `$HOME`, only the current directory is searched. The
**project root** is the directory with `root = true`, else the git toplevel, else the outermost
directory with a config file, else the current directory.

```toml
# .clankerbox.toml
agent = "claude"

[vars]
projects = ".."                          # a default users can override

[sandbox]
read = ["${projects}/frontend"]          # directories or files, relative to this file
allow = []                               # read + write
write = []
read_commands = []                       # make the directory of a $PATH command readable
network = "allow"                        # or "block"
open_ports = [3000]
env = { RUST_LOG = "debug" }
extra_args = []                          # raw nono arguments

[sandbox.nono]
mode = "supervised"                      # or "direct"
extends = []
groups = ["node_runtime"]                # `nono profile groups`

[agents.claude]
args = ["--model", "opus"]

[when.macos]                             # also: linux, wsl
sandbox.read = ["~/Library/Caches/foo"]
```

Missing paths are skipped with a warning, so a teammate without `../frontend` can still run.

### Variables

Paths and `env` values can use `${name}`. A project declares defaults in `[vars]`; these are
overridden by, highest first: `CLANKERBOX_VAR_<name>`, `.clankerbox.local.toml`, the global config.
For example, set `vars.projects = "~/work"` globally if your checkouts don't sit side by side.

Built-ins: `${project_root}`, `${config_dir}` (the current file's directory), `${home}`, `${cwd}`,
`${env:NAME}`. Any reference can have a fallback: `${name:-fallback}`.

### Agents and cb profiles

Any layer can override agent fields or define agents, e.g. `[agents.claude-opus]` with
`inherits = "claude"`.

A **cb profile** (`[profiles.<name>]`, selected with `-p <name>`) is an overlay with the
config-file format. It is separate from agents and from **nono profiles** (nono's own policy
files, set with `sandbox.nono.profile`), so one cb profile can adjust every agent.

### Example global config

```toml
# $XDG_CONFIG_HOME/clankerbox/config.toml
[sandbox]
read = ["~/notes"]                       # extra access in every project

# `cb -p personal`: a second Claude account
[profiles.personal]
sandbox.env = { CLAUDE_CONFIG_DIR = "${home}/.claude-personal" }
sandbox.allow = ["~/.claude-personal"]
```

## nono backend

- **[Execution mode](https://nono.sh/docs/cli/features/execution-modes):** `supervised`
  (default, `nono run`) or `direct` (`nono wrap`, for tools that need the agent as the foreground
  process).
- **Groups:** nono has no flag for policy groups, so cb writes a small nono profile extending the
  base one, in `$XDG_RUNTIME_DIR/clankerbox` (or `$XDG_CACHE_HOME/clankerbox`).

## License

MIT

# clankerbox

A project-aware wrapper that runs AI coding agents (Claude Code, pi, codex, …) inside a sandbox
([nono](https://nono.sh) for now).

Each project can carry a `.clankerbox.toml` saying what the agent needs beyond the repo itself:
read access to a sibling frontend repo, a dependency checked out elsewhere, extra agent flags.

```sh
cd ~/code/backend
cb init                         # creates .clankerbox.toml
$EDITOR .clankerbox.toml        # read = ["../frontend"]
cb                              # runs claude, sandboxed, with the extra access
```

> [!NOTE]
> Largely written with AI (Claude Code) under human direction and review. Check `cb show` before
> trusting what a run grants.

## Usage

```
cb [OPTIONS | SANDBOX_ARGS]... [-- AGENT_ARGS...]
```

Recognised anywhere before a lone `--`:

| Option | |
|---|---|
| `-A/--agent <name>` | Run a different agent |
| `-p/--profile <name>` | Apply a [profile](#profiles) (repeatable) |
| `--nono-profile <name>` | Override the configured nono profile |
| `-C <dir>` | Run as if started in `<dir>` |
| `--no-project` | Ignore project config files |
| `--unsandboxed` | Run without a sandbox (or set `sandbox.backend = "none"`) |

Every other argument before `--` goes to the sandbox backend verbatim, e.g. `--allow ./dir`.
Arguments after `--` go to the agent, after any configured agent args.

```sh
cb -A pi -p personal
cb --allow ./scratch -- --resume    # one-off grant, plus an agent flag
cb show                             # what would run, and why
```

| Subcommand | |
|---|---|
| `init [--local]` | Create `.clankerbox.toml`, or a personal `.clankerbox.local.toml` added to `.git/info/exclude` |
| `trust` / `untrust` | Approve or revoke the project config files that apply here |
| `show [--json] [ARGS]` | Config files, grants with their source, and the final command |
| `agents` | Available agents (`*` marks the default) and profiles |
| `edit [--local]` | Open the nearest config file in `$EDITOR`, then trust it |
| `doctor` | Check nono, the agent command, profiles and config files |
| `schema` | JSON Schema for config files |
| `completions <shell>` | Shell completions |

## Configuration

Layers, lowest precedence first:

1. Built-in presets: `claude` (the default), `codex`, `omp`, `opencode`, `pi` ([`src/presets.toml`](src/presets.toml))
2. Global config: `$XDG_CONFIG_HOME/clankerbox/config.toml`
3. Each `.clankerbox.toml` from the project root down to the current directory, each followed
   by its `.clankerbox.local.toml`
4. Profiles selected with `-p`, in order
5. Command-line arguments

How layers combine:

- Lists are appended and single values replaced.
- Everything is additive: a project can grant more access but never remove any.
- Unknown keys are errors.
- More specific settings win over general ones, lowest precedence first:
  1. top-level `[sandbox]`
  2. `[agents.<parent>]`
  3. `[agents.<name>]`
  4. the same three for each profile

  Within each step, later files win. A top-level `sandbox.nono.profile` is only a default for
  agents that don't set their own.

Config files are searched from the current directory up to the git toplevel. Outside git, the
search stops below `$HOME`; outside `$HOME`, only the current directory is searched.

The **project root** is resolved in this order: the directory with `root = true`, the git
toplevel, the outermost directory with a config file, the current directory. The agent gets
read-write access to it, unless it is `$HOME` or contains it. In a linked worktree the agent also
gets the shared git directory.

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
extra_args = []                          # raw backend arguments

[sandbox.nono]
mode = "supervised"                      # or "direct"
extends = []
groups = ["node_runtime"]                # `nono profile groups`

[agents.claude]
args = ["--model", "opus"]
sandbox.nono.profile = "claude-code"

[when.macos]                             # also: linux, wsl
sandbox.read = ["~/Library/Caches/foo"]
```

Missing paths are skipped with a warning, so a teammate without `../frontend` can still run.

### Variables

Paths and `env` values can use `${name}`. A project declares defaults in `[vars]`. They can be
overridden, from highest precedence to lowest, by:

1. `CLANKERBOX_VAR_<name>` in the environment
2. `.clankerbox.local.toml`
3. the global config

For example, set `vars.projects = "~/work"` globally if your checkouts don't sit side by side.

Built-in variables:
- `${project_root}`
- `${config_dir}`, the directory of the current file
- `${home}`
- `${cwd}`
- `${env:NAME}`

Any reference can have a fallback: `${name:-fallback}`.

### Agents and profiles

Any layer can override agent fields or define agents, e.g.
`[agents.claude-opus]` with `inherits = "claude"`. `kind = "claude"` (inherited) also grants the
main worktree's `.claude/settings.local.json` in linked worktrees.

A **profile** (`[profiles.<name>]`) is an overlay with the config-file format, applied when you
pass `-p <name>`. Profiles are separate from agents, so one `personal` profile can adjust every
agent; see the example below.

## Trust

A repository's config can grant access to anything, so project files must be approved, like
`direnv allow`:
- A new or changed file shows its contents and grants, and asks before running.
- Without a terminal, cb refuses until you run `cb trust`.
- Hashes are stored in `$XDG_STATE_HOME/clankerbox/trust.toml`.
- `init` and `edit` trust what they write. The global config is always trusted.

## nono backend

- **[Execution mode](https://nono.sh/docs/cli/features/execution-modes):** `supervised`
  (default, `nono run`) or `direct` (`nono wrap`, for tools like Herdr that need the agent as the
  foreground process).
- **Grants:** they become `--read`/`--write`/`--allow` (or the `-file` variants), `--block-net`,
  `--open-port` and `--listen-port`.
- **Groups:** nono has no flag for policy groups, so cb writes a small profile that extends the
  base profile. It goes in a private directory: `$XDG_RUNTIME_DIR/clankerbox`, or
  `$XDG_CACHE_HOME/clankerbox`.
- **WSL:** `read_commands` directories are held open for the agent's lifetime, because on 9p
  mounts like `/mnt/c` Landlock rules stop matching once the dentry is evicted.
- **Environment:** nono passes the environment through unless the profile filters it
  (`environment.allow_vars`/`deny_vars`).

## Migrating from a shell wrapper

Wrappers like `nclaude`, `nclaude-personal`, `npi` and `nomp` become one global config. Worktree
handling is built in.

```toml
# ~/.config/clankerbox/config.toml
[agents.claude]
sandbox.nono.profile = "nclaude"
sandbox.nono.mode = "direct"             # for Herdr
sandbox.env = { HERDR_AGENT = "claude" }

[agents.pi]
sandbox.nono.profile = "npi"

[agents.omp]
sandbox.nono.profile = "nomp"

[profiles.personal]
sandbox.env = { CLAUDE_CONFIG_DIR = "${home}/.claude-personal" }

[profiles.personal.agents.claude]
sandbox.nono.profile = "nclaude-personal"

[profiles.personal.agents.pi]
sandbox.nono.profile = "npi-personal"

[when.wsl.sandbox]
nono.extends = ["wsl"]
read_commands = ["orca.exe", "powershell.exe", "op-ssh-sign-wsl.exe", "ssh.exe"]
```

| Before | After |
|---|---|
| `nclaude` / `nclaude-personal` | `cb` / `cb -p personal` |
| `npi` / `npi-personal` | `cb -A pi` / `cb -A pi -p personal` |
| `nclaude --read ~/x -- --resume` | `cb --read ~/x -- --resume` |

## Development

```sh
nix develop path:. -c cargo test
nix build path:.
```

Commits follow [Conventional Commits](https://www.conventionalcommits.org), enforced by the **CI - Lint and test** workflow.
To release, run the **Dispatch - Release** workflow from the Actions tab. It:

1. picks the next [semver](https://semver.org) version from the commits (or uses one you give);
2. updates `Cargo.toml` and `CHANGELOG.md` with [git-cliff](https://git-cliff.org), then tags;
3. publishes a GitHub release with `cb` built for x86_64 and arm64 Linux (static musl) and macOS.

Sandbox backends live in `src/sandbox/`, one file each, implementing the `Sandbox` trait. Adding
one also means adding a `BackendKind` variant.

## License

MIT

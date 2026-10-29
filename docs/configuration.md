# Configuration

`cb schema` prints the full format as JSON Schema.

## Terms

- **nono profile:** nono's policy file describing what an agent needs, e.g. its config directory.
  Every run starts from one.
- **Group:** a named set of grants that nono provides, e.g. `rust_runtime`, `node_runtime`.
- **cb profile:** a named overlay in cb's own config, selected with `-p`, e.g. to use a second
  account. Unrelated to nono profiles.
- **Project root:** the directory with `root = true` in its `.clankerbox.toml`, else the git
  toplevel, else the outermost directory with a config file, else the current directory.

## Layers

Lowest precedence first:

1. Built-in agent presets: `claude` (the default), `codex`, `omp`, `opencode`, `pi`
   ([`src/presets.toml`](../src/presets.toml))
2. Global config: `$XDG_CONFIG_HOME/clankerbox/config.toml`
3. Each `.clankerbox.toml` from the project root down to the current directory, each followed
   by its `.clankerbox.local.toml`
4. cb profiles selected with `-p`, in order
5. Command-line arguments

Merging:

- Lists are appended, single values replaced. A later layer can add access but never remove it.
- Unknown keys are errors.
- More specific settings win, lowest first: top-level `[sandbox]`, the agent it `inherits` from,
  the agent itself, then the same for each cb profile. Within each step, later files win.

cb looks for config files from the current directory up to the git toplevel. Outside git, the
search stops below `$HOME`; outside `$HOME`, only the current directory is searched.

## Example project config

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
groups = ["node_runtime"]                # list them with `nono profile groups`

[agents.claude]
args = ["--model", "opus"]

[when.macos]                             # also: linux, wsl
sandbox.read = ["~/Library/Caches/foo"]
```

Missing paths are skipped with a warning.

## Example global config

```toml
# $XDG_CONFIG_HOME/clankerbox/config.toml
approve_detected = true                  # don't ask in projects without a config file

[sandbox]
read = ["~/notes"]                       # extra access in every project

# `cb -p personal`: a second Claude account
[profiles.personal]
sandbox.env = { CLAUDE_CONFIG_DIR = "${home}/.claude-personal" }
sandbox.allow = ["~/.claude-personal"]
```

## Variables

Paths and `env` values can use `${name}`. A project declares defaults in `[vars]`, overridden
by, highest first: `CLANKERBOX_VAR_<name>`, `.clankerbox.local.toml`, the global config. For
example, set `vars.projects = "~/work"` globally if your checkouts don't sit side by side.

Built-ins: `${project_root}`, `${config_dir}` (the current file's directory), `${home}`, `${cwd}`,
`${env:NAME}`. Any reference can have a fallback: `${name:-fallback}`.

## Agents and cb profiles

Any layer can change an agent's settings or define a new one, e.g. `[agents.claude-opus]` with
`inherits = "claude"`.

A cb profile (`[profiles.<name>]`, selected with `-p <name>`) uses the same format as a config
file and is applied on top of it. One cb profile can adjust every agent.

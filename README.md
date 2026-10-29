# clankerbox

`cb` runs AI coding agents (Claude Code, pi, codex, …) in a sandbox, so they can only touch the
files you allow. Linux and macOS.

An agent working in your repo runs commands on your machine, with your permissions. The
sandbox, [nono](https://nono.sh), uses the operating system's kernel (Landlock on Linux, Seatbelt
on macOS) to confine it to the project, its toolchain and the agent's own config. Everything else
is off limits, even to commands the agent starts.

You can call nono directly, but then you pass the right flags on every run. `cb` works them out:

- it starts from a ready-made sandbox policy for the agent,
- detects the project's toolchains (Rust, Node, Java, …) and adds what they need,
- reads a `.clankerbox.toml` in the project for anything else, such as a sibling repo the agent
  should read,
- and asks you to approve all of it before the first run.

> [!NOTE]
> Largely written with AI (Claude Code) under human direction and review. Check `cb show` before
> trusting what a run grants.

## Installation

On Linux or macOS, install the latest release with:

```sh
curl -fsSL https://raw.githubusercontent.com/Fumler/clankerbox/main/install.sh | bash
```

It installs `cb` to `~/.local/bin` on Linux or `/usr/local/bin` on macOS (using `sudo` if that
isn't writable). Set `CB_INSTALL_DIR` to install elsewhere:

```sh
curl -fsSL https://raw.githubusercontent.com/Fumler/clankerbox/main/install.sh | CB_INSTALL_DIR=~/bin bash
```

With Nix:

```sh
nix profile install github:Fumler/clankerbox   # or try it: nix run github:Fumler/clankerbox
```

Or add it as a flake input:

```nix
inputs.clankerbox.url = "github:Fumler/clankerbox";

# in environment.systemPackages or home.packages:
inputs.clankerbox.packages.${pkgs.stdenv.hostPlatform.system}.default
```

To reuse your own nixpkgs, add `inputs.clankerbox.inputs.nixpkgs.follows = "nixpkgs";`. Its Rust
must be at least the `rust-version` in `Cargo.toml`, which usually means nixpkgs unstable.

Archives for each platform are also on the
[releases page](https://github.com/Fumler/clankerbox/releases).

## Quickstart

1. Install [nono](https://nono.sh). Claude Code needs nothing else: cb ships its nono profile.
   For other agents, install theirs:

   ```sh
   nono pull nolabs-ai/pi          # or nolabs-ai/omp
   ```

   If you skip this, `cb` offers to run it for you.

2. [Install `cb`](#installation), then check the setup with `cb doctor`.

3. Run it in a project. With no config at all, `cb` runs Claude Code sandboxed to that project:

   ```sh
   cb                              # asks once to approve what it will grant, then runs claude
   cb --allow ./scratch            # one-off extra access
   cb show                         # what a run can access, and why
   ```

4. Optionally, save extra access the project always needs:

   ```sh
   cb init                         # write .clankerbox.toml, including detected toolchains
   $EDITOR .clankerbox.toml        # e.g. read = ["../frontend"]
   cb                              # asks once to trust the new file, then runs
   ```

Your own defaults (preferred agent, access you want everywhere) go in an optional
[global config](#example-global-config).

## Terms

- **Grant:** something the sandbox lets the agent do, such as read a directory or write to one.
  Anything not granted is denied.
- **nono profile:** nono's policy file describing what an agent needs, e.g. its config directory.
  Every run starts from one.
- **Group:** a named set of grants that nono provides, e.g. `rust_runtime`, `node_runtime`.
- **cb profile:** a named overlay in cb's own config, selected with `-p`, e.g. to use a second
  account. Unrelated to nono profiles.
- **Project root:** where the project starts: the directory with `root = true` in its
  `.clankerbox.toml`, else the git toplevel, else the outermost directory with a config file,
  else the current directory.

## Usage

```
cb [OPTIONS | SANDBOX_ARGS]... [-- AGENT_ARGS...]
```

Options, recognised anywhere before a lone `--`:

| Option | |
|---|---|
| `-A/--agent <name>` | Run a different agent |
| `-p/--profile <name>` | Apply a [cb profile](#agents-and-cb-profiles) (repeatable) |
| `--nono-profile <name>` | Override the nono profile |
| `-C <dir>` | Run as if started in `<dir>` |
| `--no-project` | Ignore project config files |
| `--unsandboxed` | Run without a sandbox (or set `sandbox.backend = "none"`) |
| `--dry-run` | Print the command instead of running it |
| `-y/--yes` | Without a project config, run with the [detected grants](#detected-toolchains) without asking |
| `--detect-subdirs` | Search subdirectories for toolchains outside a git or jj repository too |
| `--nono-<flag>` | Pass `--<flag>` to nono, for flags cb uses itself, e.g. `--nono-dry-run` |

Other arguments before `--` go to nono unchanged, e.g. `--allow ./dir`. Arguments after `--` go
to the agent, e.g. `cb -- --model opus`.

| Subcommand | |
|---|---|
| `init [--local] [--force]` | Create `.clankerbox.toml`, or a personal `.clankerbox.local.toml` that git ignores |
| `trust` / `untrust` | Approve or revoke the project config files that apply here |
| `show [--json] [ARGS]` | Config files, grants with their source, every rule nono will apply, and the final command |
| `agents` | Available agents (`*` marks the default) and cb profiles |
| `edit [--local]` | Open the nearest config file in `$EDITOR`, then trust it |
| `doctor` | Check nono, the agent command, nono profiles and config files |
| `schema` | JSON Schema for config files |
| `completions <shell>` | Shell completions |

## What the agent can access

Network access and environment variables follow the nono profile: by default, outbound network
is allowed and your environment is passed through.

### Always

- **The agent's nono profile:**
  - `claude` uses `cb:claude`, built into cb
    ([`src/sandbox/profiles/claude.json`](src/sandbox/profiles/claude.json)) and adapted from
    nono's `nolabs-ai/claude` pack. cb creates the directories it grants before each run. Set
    `sandbox.nono.profile = "claude-code"` to use the pack instead.
  - `pi` and `omp` use the profiles of the same name, installed with `nono pull`.
  - `codex` and `opencode` use nono's built-in defaults.
- **Project root:** read-write, unless it is `$HOME` or contains it.
- **Current directory:** `--allow-cwd`, at the level the nono profile sets.
- **Linked git worktrees:** the shared git directory, and for `claude` the main worktree's
  `.claude/settings.local.json`.
- **Linux only:** the nono group `linux_temp_read`. In direct mode also read access to `/proc`:
  `nono wrap` grants `/proc/self` only to the first process, and child processes like Bun or
  browsers need it. This also makes other processes' command lines readable.

### Claude Code

- **Config directory:** read-write access to `$CLAUDE_CONFIG_DIR` and its `.lock` file when it
  is set (in `sandbox.env` or your environment).
- **Symlinked config:** read-only access to the targets of symlinks in the config directory and
  its `skills`, `agents`, `commands` and `output-styles` directories, e.g. a `settings.json` from
  your dotfiles. Grant `allow` on a target yourself if Claude should be able to change it.
  - Targets that make up a whole directory are granted as that directory.
  - Symlinks are only followed in a config directory from your environment, the global config or
    the default `~/.claude`, never one a project file sets.
  - `$HOME` itself is never granted this way.
- **Denial hints:** cb loads a Claude Code plugin for the session (`--plugin-dir`, read-only in
  the sandbox). When a command fails with a permission error, its hooks ask Claude to confirm the
  cause with `nono why` and suggest the `cb` flag or config entry that would fix it. Your own
  plugins still load. The hooks run `cb hook explain-denial`, so the sandbox can also read the
  `cb` binary. Turn this off with `sandbox.nono.explain_denials = false`.
- **Herdr socket:** inside a [Herdr](#herdr) pane, connect access to `$HERDR_SOCKET_PATH`.

### Detected toolchains

cb looks for marker files such as `Cargo.toml` or `pom.xml` in the project root. In a git or jj
repository it also searches up to three directories below it, to cover monorepos;
`--detect-subdirs` does this outside a repository too. Dependency, build and hidden directories
are skipped.

For each toolchain found, cb adds nono [groups](https://nono.sh/docs/cli/features/profiles-groups)
(e.g. `rust_runtime`) and write access to its shared package caches (e.g. `~/.cargo/registry`).

- `cb init` writes these into `.clankerbox.toml`, where you can review and edit them.
- Without any project config file, `cb` applies them directly and asks you to approve them (see
  [Trust](#trust)).

## Trust

A config file in a repository can grant access to anything, so cb won't use one until you approve
it, like `direnv allow`.

- **New or changed config file:** the first run shows its contents and grants and asks
  `Trust these files and continue? [y/N]`. Without a terminal, cb refuses until you run
  `cb trust`. `init` and `edit` trust what they write. The global config is always trusted.
- **No config file:** the first run lists every grant, including detected ones, and asks
  `Run with these grants?`. cb remembers the answer for the project root until the detected
  toolchains change. Skip the question with `-y`, or everywhere with `approve_detected = true` at
  the top of the global config (ignored, with a warning, anywhere else).

## Configuration

Settings come from several layers, lowest precedence first:

1. Built-in agent presets: `claude` (the default), `codex`, `omp`, `opencode`, `pi`
   ([`src/presets.toml`](src/presets.toml))
2. Global config: `$XDG_CONFIG_HOME/clankerbox/config.toml`
3. Each `.clankerbox.toml` from the project root down to the current directory, each followed
   by its `.clankerbox.local.toml`
4. cb profiles selected with `-p`, in order
5. Command-line arguments

How layers combine:

- Lists are appended, single values replaced. A later layer can add access but never remove it.
- Unknown keys are errors.
- More specific settings win, lowest first: top-level `[sandbox]`, the agent it `inherits` from,
  the agent itself, then the same for each cb profile. Within each step, later files win.

cb looks for config files from the current directory up to the git toplevel. Outside git, the
search stops below `$HOME`; outside `$HOME`, only the current directory is searched.

### Example project config

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

Missing paths are skipped with a warning, so a teammate without `../frontend` can still run.
`cb schema` prints the full format.

### Variables

Paths and `env` values can use `${name}`. A project declares defaults in `[vars]`; these are
overridden by, highest first: `CLANKERBOX_VAR_<name>`, `.clankerbox.local.toml`, the global config.
For example, set `vars.projects = "~/work"` globally if your checkouts don't sit side by side.

Built-ins: `${project_root}`, `${config_dir}` (the current file's directory), `${home}`, `${cwd}`,
`${env:NAME}`. Any reference can have a fallback: `${name:-fallback}`.

### Agents and cb profiles

Any layer can change an agent's settings or define a new agent, e.g. `[agents.claude-opus]` with
`inherits = "claude"`.

A cb profile (`[profiles.<name>]`, selected with `-p <name>`) uses the same format as a config
file and is applied on top of it. Because it is separate from agents, one cb profile can adjust
every agent.

### Example global config

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

## How cb drives nono

- **[Execution mode](https://nono.sh/docs/cli/features/execution-modes):** `supervised`
  (default, `nono run`) or `direct` (`nono wrap`, for tools that need the agent as the foreground
  process).
- **Groups and built-in profiles:** nono has no flag for policy groups, so cb writes a small nono
  profile extending the base one to `$XDG_RUNTIME_DIR/clankerbox` (or `$XDG_CACHE_HOME/clankerbox`).
  nono can't extend a profile file, so for a built-in `cb:` profile or a profile file (a path,
  relative to the directory the agent runs in) cb writes a copy with the groups added instead.

## Herdr

[Herdr](https://herdr.dev) finds agents by their pane's foreground process, which in supervised
mode is nono rather than Claude. Its own Claude integration also resumes sessions with plain
`claude --resume`, outside the sandbox. So when `cb` starts Claude Code in a Herdr pane
(`HERDR_ENV=1`), in either mode, cb's Claude Code plugin reports the agent itself:

- **State:** working, blocked on a permission prompt, or idle, from Claude Code's hooks
  (`cb hook herdr`). Subagents' events are ignored.
- **Resume:** after a Herdr restart, the pane comes back through `cb ... -- --resume <session>`
  with the options that affect the session (`-A`, `-p`, `--nono-profile`, the directory,
  sandbox arguments), so it keeps its sandbox and cb profile. Agent arguments after `--` are
  not kept, since a prompt among them would be sent again; put lasting ones in the agent's
  `args`. `-y` is not kept either, so new detected grants still need approval.
  `CLAUDE_CONFIG_DIR` and `CLANKERBOX_VAR_*` from your environment are passed along with
  `env`. A command Herdr wouldn't accept (an argument with `'`, over 64 arguments or 8 KiB) is
  left out; state is still reported.
- **Herdr's own Claude hook:** once it reports a session, Herdr ignores cb's reports for the
  pane. After you approve a run inside Herdr, cb adds one line to that hook
  (`herdr-agent-state.sh`, found through the hook commands in Claude's `settings.json`) so it
  skips sessions cb reports. Other Claude sessions are unaffected, and the agent still sees
  `HERDR_*`. cb only edits Herdr's own copy: never through a symlink, and only a script with
  Herdr's Claude integration header; otherwise it warns.

This needs Herdr 0.9.2 or later and an agent with `kind = "claude"`. Outside Herdr, cb adds
nothing.

> [!WARNING]
> Reporting needs the Herdr socket, so the sandbox can connect to it, and anything that can
> connect to it can control every Herdr pane, e.g. type into an unsandboxed shell. On Linux,
> nono's sandbox could already connect to it without this grant. Turn the integration off with
> `sandbox.nono.herdr = false`.

`HERDR_AGENT` only helps Herdr's own process and screen detection, which can't see the agent
in supervised mode. You can leave it set, but cb's reports don't need it.

## License

MIT

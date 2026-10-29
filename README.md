# clankerbox

`cb` runs AI coding agents (Claude Code, codex, pi, …) in a [nono](https://nono.sh) sandbox, so
they can only touch the files you allow. Linux and macOS.

## How it works

A coding agent runs commands on your machine with your permissions. nono uses the kernel
(Landlock on Linux, Seatbelt on macOS) to deny everything it hasn't been granted, including to
commands the agent starts. A **grant** is one permission, such as reading or writing a
directory.

nono needs the right flags on every run. `cb` works them out:

1. Starts from a sandbox profile for the agent.
2. Adds what the project's toolchains (Rust, Node, Java, …) need.
3. Adds anything in the project's `.clankerbox.toml`, such as a sibling repo to read.
4. Asks you to approve the result before the first run.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/Fumler/clankerbox/main/install.sh | bash
```

Installs to `~/.local/bin` (Linux) or `/usr/local/bin` (macOS, with `sudo` if needed). Set
`CB_INSTALL_DIR` to change it:

```sh
curl -fsSL https://raw.githubusercontent.com/Fumler/clankerbox/main/install.sh | CB_INSTALL_DIR=~/bin bash
```

With Nix:

```sh
nix profile install github:Fumler/clankerbox   # or try it: nix run github:Fumler/clankerbox
```

As a flake input:

```nix
inputs.clankerbox.url = "github:Fumler/clankerbox";

# in environment.systemPackages or home.packages:
inputs.clankerbox.packages.${pkgs.stdenv.hostPlatform.system}.default
```

`inputs.clankerbox.inputs.nixpkgs.follows = "nixpkgs";` reuses your nixpkgs. Its Rust must be at
least the `rust-version` in `Cargo.toml`, usually meaning nixpkgs unstable.

Archives for each platform are on the [releases page](https://github.com/Fumler/clankerbox/releases).

## Quickstart

1. Install [nono](https://nono.sh). Claude Code needs nothing else: cb ships its nono profile.
   Other agents need theirs (`cb` offers to run this if you skip it):

   ```sh
   nono pull nolabs-ai/pi          # or nolabs-ai/omp
   ```

2. Install `cb`, then check the setup with `cb doctor`.

3. Run it in a project. With no config, `cb` runs Claude Code sandboxed to the project:

   ```sh
   cb                              # approve the grants once, then runs claude
   cb --allow ./scratch            # one-off extra access
   cb show                         # what a run can access, and why
   ```

4. Optionally, save access the project always needs:

   ```sh
   cb init                         # write .clankerbox.toml, including detected toolchains
   $EDITOR .clankerbox.toml        # e.g. read = ["../frontend"]
   cb                              # approve the new file once, then runs
   ```

Personal defaults (preferred agent, access you want everywhere) go in the
[global config](docs/configuration.md#example-global-config).

## Security and caveats

> [!NOTE]
> cb is largely written with AI (Claude Code) under human direction and review. Check `cb show`
> before trusting what a run grants.

- **Network and environment are open by default.** Outbound network is allowed and your whole
  environment is passed through, including any secrets in it. Both follow the nono profile;
  block network with `sandbox.network = "block"`.
- **The project root is read-write**, unless it is `$HOME` or contains it.
- **Config files need approval.** A `.clankerbox.toml` can grant anything, so cb won't use a new
  or changed one until you approve it, like `direnv allow`. The global config is always trusted.
  See [Trust](#trust).
- **Linux direct mode makes `/proc` readable**, including other processes' command lines.
- **Herdr:** inside a [Herdr](https://herdr.dev) pane, the sandbox can connect to Herdr's
  socket, and through it control every pane, including unsandboxed shells. Turn it off with
  `sandbox.nono.herdr = false`. See [docs/herdr.md](docs/herdr.md).

Everything a run grants, and why: [docs/sandbox.md](docs/sandbox.md).

### Trust

- **New or changed config file:** the first run shows its contents and grants and asks
  `Trust these files and continue? [y/N]`. Without a terminal, cb refuses until you run
  `cb trust`. `cb init` and `cb edit` trust what they write.
- **No config file:** the first run lists every grant, including detected ones, and asks
  `Run with these grants?`. cb remembers the answer for the project root until the detected
  toolchains change. Skip the question with `-y`, or everywhere with `approve_detected = true` at
  the top of the global config (ignored, with a warning, anywhere else).

## Usage

```
cb [OPTIONS | SANDBOX_ARGS]... [-- AGENT_ARGS...]
```

Options, recognised anywhere before a lone `--`:

| Option | |
|---|---|
| `-A/--agent <name>` | Run a different agent |
| `-p/--profile <name>` | Apply a [cb profile](docs/configuration.md#agents-and-cb-profiles) (repeatable) |
| `--nono-profile <name>` | Override the nono profile |
| `-C <dir>` | Run as if started in `<dir>` |
| `--no-project` | Ignore project config files |
| `--unsandboxed` | Run without a sandbox (or set `sandbox.backend = "none"`) |
| `--dry-run` | Print the command instead of running it |
| `-y/--yes` | Without a project config, run with the [detected grants](docs/sandbox.md#detected-toolchains) without asking |
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

## Configuration

Settings are layered: built-in agent presets, the global config
(`$XDG_CONFIG_HOME/clankerbox/config.toml`), each `.clankerbox.toml` and
`.clankerbox.local.toml` from the project root down, cb profiles (`-p`), then the command line.
Lists are appended and single values replaced, so a later layer can add access but never remove
it.

```toml
# .clankerbox.toml
agent = "claude"

[sandbox]
read = ["../frontend"]                   # relative to this file
allow = ["../shared"]                    # read + write
open_ports = [3000]

[sandbox.nono]
groups = ["node_runtime"]                # list them with `nono profile groups`

[agents.claude]
args = ["--model", "opus"]
```

Missing paths are skipped with a warning, so a teammate without `../frontend` can still run.
`cb schema` prints the full format; [docs/configuration.md](docs/configuration.md) covers every
key, precedence, variables and cb profiles.

## More

- [docs/configuration.md](docs/configuration.md): config layers, variables, agents and cb profiles
- [docs/sandbox.md](docs/sandbox.md): what the agent can access, and how cb drives nono
- [docs/herdr.md](docs/herdr.md): Herdr integration
- [CONTRIBUTING.md](CONTRIBUTING.md): building, testing and releasing
- [CHANGELOG.md](CHANGELOG.md)

## License

MIT

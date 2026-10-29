# clankerbox

Rust CLI (`cb`) that runs AI coding agents inside a sandbox (nono), configured per project.
See README.md for an overview and `docs/` for config and sandbox details.

## Commands

No system Rust toolchain or C linker; use the flake's dev shell:

```sh
nix develop path:. -c cargo test
nix develop path:. -c cargo clippy --all-targets -- -D warnings
nix develop path:. -c cargo fmt
nix build path:.
```

`path:.` is needed because nix only sees git-tracked files otherwise. The `.envrc` loads the same
shell via direnv for humans; tool shells don't load it, so keep using `nix develop`.

## Rules

- This is a jj repo (colocated with git): use `jj`, not `git`, for anything that changes history.
- Commit messages follow Conventional Commits (`feat:`, `fix:`, `feat!:` …). The CI - Lint and
  test workflow rejects others, and git-cliff derives the next version and `CHANGELOG.md` from them.
  Preview with `nix develop path:. -c git cliff --unreleased`.
- Update README.md and `docs/` in the same change as user-visible behaviour. README is for
  newcomers (what, install, use, risks); reference detail goes in `docs/`. Keep both terse: no
  filler, one fact per bullet, no repeating a fact across files.
- `///` docs on `src/config/model.rs` and `src/cli.rs` types become `cb schema` and `--help`
  text; write them for users.
- Sandbox backends go in `src/sandbox/` behind the `Sandbox` trait. Keep backend-specific logic
  out of `config/` and `policy.rs`.
- Grants are security-relevant: add a test for any change to discovery, trust, merge precedence
  or the built-in grants.
- Every trust prompt is built by `review_text` in `src/commands/run.rs`. Put anything users should
  see before approving in its shared tail (not one prompt kind), and show it in `cb show` too.
- Comment only what the code can't say (a why, a constraint, provenance), in a few words. No
  comments restating names, types or the next line, and none explaining choices from an agent
  chat that make no sense without it (e.g. "instead of X", "as discussed").
- Integration tests in `tests/cli.rs` run `cb` against a stub `nono` on `PATH`; real nono can't
  run inside a nono sandbox.

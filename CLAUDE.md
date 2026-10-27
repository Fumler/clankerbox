# clankerbox

Rust CLI (`cb`) that runs AI coding agents inside a sandbox (nono), configured per project.
See README.md for behaviour and config format.

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
- `///` docs on `src/config/model.rs` and `src/cli.rs` types become `cb schema` and `--help`
  text; write them for users.
- Sandbox backends go in `src/sandbox/` behind the `Sandbox` trait. Keep backend-specific logic
  out of `config/` and `policy.rs`.
- Grants are security-relevant: add a test for any change to discovery, trust, merge precedence
  or the built-in grants.
- Integration tests in `tests/cli.rs` run `cb` against a stub `nono` on `PATH`; real nono can't
  run inside a nono sandbox.

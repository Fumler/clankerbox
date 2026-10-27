# Contributing

```sh
nix develop path:. -c cargo test
nix build path:.
```

Commits follow [Conventional Commits](https://www.conventionalcommits.org), enforced by the
**CI - Lint and test** workflow. Preview the changelog with
`nix develop path:. -c git cliff --unreleased`.

## Releasing

Run the **Dispatch - Release** workflow. It picks the next [semver](https://semver.org) version
from the commits (or uses the one you give), updates `CHANGELOG.md` with
[git-cliff](https://git-cliff.org), tags, and publishes `cb` for x86_64 and arm64 Linux and macOS.

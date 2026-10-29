# Changelog

All notable changes to this project are documented here. The format follows
[Conventional Commits](https://www.conventionalcommits.org) and versions follow
[Semantic Versioning](https://semver.org).

## [2.1.0] - 2026-10-29

### Features

- **herdr:** Report Claude sessions to Herdr and resume them through cb

### Miscellaneous

- **install:** Add install script and installation docs [skip ci]
## [2.0.0] - 2026-10-28

### Features

- **run:** **BREAKING** Confirm detected grants in projects without a config
- **nono:** **BREAKING** Ship a built-in Claude Code profile and denial hints

### Documentation

- **claude:** Add a comment rule to CLAUDE.md

### Testing

- **cli:** Drop dotfiles-specific tests and stop asserting full nono argv

### Miscellaneous

- **dev:** Add the project's cb config and ignore local overrides
## [1.1.0] - 2026-10-27

### Features

- **cli:** Colour terminal output and --help

### Miscellaneous

- **dev:** Add direnv .envrc for the dev shell
## [1.0.0] - 2026-10-27

### Features

- **cli:** Add cb, a project-aware sandbox wrapper for AI agents
- **nono:** Default to supervised mode with direct override
- **init:** Grant toolchain groups and caches on init
- **nono:** Check nono and offer to pull missing profiles before run
- **cli:** Add --dry-run and --nono-<flag> passthrough

### Documentation

- **readme:** Fix broken profile link
- **readme:** Rewrite for new users and add CONTRIBUTING.md

### Testing

- **cli:** Cover nclaude wrapper replacements

### Miscellaneous

- **config:** Shorten comments

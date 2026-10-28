#!/usr/bin/env bash
set -euo pipefail

main() {
  repo="Fumler/clankerbox"

  case "$(uname -s)" in
    Linux) os="unknown-linux-musl" dir="${CB_INSTALL_DIR:-$HOME/.local/bin}" ;;
    Darwin) os="apple-darwin" dir="${CB_INSTALL_DIR:-/usr/local/bin}" ;;
    *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
  esac

  case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac

  latest="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")"
  version="${latest##*/}"
  archive="cb-$version-$arch-$os"

  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  echo "Downloading cb $version"
  curl -fsSL "https://github.com/$repo/releases/download/$version/$archive.tar.gz" | tar -xz -C "$tmp"

  sudo=""
  mkdir -p "$dir" 2>/dev/null && [[ -w "$dir" ]] || sudo="sudo"
  $sudo mkdir -p "$dir"
  $sudo install -m 755 "$tmp/$archive/cb" "$dir/cb"

  echo "Installed cb $version to $dir/cb"

  case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add $dir to your PATH to run cb" ;;
  esac
}

main "$@"

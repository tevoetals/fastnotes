#!/usr/bin/env bash
# Gera dist/fastnotes-x86_64-linux.tar.gz: binários compilados num Ubuntu
# 22.04 (glibc 2.35), que rodam em Kubuntu/Ubuntu ≥ 22.04, Debian 12, Fedora,
# Arch etc. Precisa de Docker. Usado pelo release.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=dist/fastnotes-x86_64-linux.tar.gz
mkdir -p dist
git archive --format=tar HEAD > dist/src.tar
docker run --rm -v "$PWD/dist:/dist" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" ubuntu:22.04 bash -euo pipefail -c '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq && apt-get install -y -qq curl build-essential pkg-config libxkbcommon-dev >/dev/null
  curl --proto "=https" -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal >/dev/null
  . "$HOME/.cargo/env"
  mkdir /src && tar xf /dist/src.tar -C /src && cd /src
  cargo build --release -q
  P=/tmp/pkg/fastnotes; mkdir -p $P/bin
  cp target/release/fastnotes target/release/fastnotes-telegram $P/bin/
  cp -r fonts data install.sh README.md LICENSE $P/
  rm -rf $P/data/screenshots
  tar czf /dist/fastnotes-x86_64-linux.tar.gz -C /tmp/pkg fastnotes
  chown "$HOST_UID:$HOST_GID" /dist/fastnotes-x86_64-linux.tar.gz
'
rm -f dist/src.tar
echo "✔ $OUT ($(du -h "$OUT" | cut -f1))"

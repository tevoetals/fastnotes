#!/usr/bin/env bash
# Publica uma versão: Cargo.toml + metainfo + tag no GitHub + manifesto no Flathub.
#   ./release.sh 0.3.0 "O que mudou nesta versão"
# Depois do push, o Flathub compila e publica sozinho (aparece no Discover em ~1 h).
set -euo pipefail
cd "$(dirname "$0")"
ID=io.github.tevoetals.fastnotes
VER=${1:?versão, ex.: 0.3.0}
NOTES=${2:?descrição curta da versão}
FLATHUB=${FLATHUB_DIR:-$HOME/Programs/flathub-$ID}
[[ -z "$(git status --porcelain)" ]] || { echo "árvore suja: commit antes"; exit 1; }
DATE=$(date +%F)

# 1. versão + changelog
sed -i "0,/^version = \"[^\"]*\"/s//version = \"$VER\"/" Cargo.toml
cargo update -w -q
python3 - "$VER" "$DATE" "$NOTES" <<'PY'
import sys
ver,date,notes=sys.argv[1:4]
p='data/io.github.tevoetals.fastnotes.metainfo.xml'; s=open(p).read()
esc=notes.replace('&','&amp;').replace('<','&lt;')
rel=f'    <release version="{ver}" date="{date}">\n      <description>\n        <p>{esc}</p>\n      </description>\n    </release>\n'
s=s.replace('  <releases>\n','  <releases>\n'+rel,1); open(p,'w').write(s)
PY
# 2. fontes offline do cargo (o Flathub compila sem rede)
CACHE=${XDG_CACHE_HOME:-$HOME/.cache}/fastnotes-release
VENV=$CACHE/venv
if [[ ! -x $VENV/bin/python ]]; then
  python3 -m venv "$VENV"; "$VENV/bin/pip" -q install "aiohttp<4" pyyaml tomlkit
fi
GEN=$CACHE/flatpak-cargo-generator.py
[[ -f $GEN ]] || curl -sSL -o "$GEN" https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/master/cargo/flatpak-cargo-generator.py
"$VENV/bin/python" "$GEN" Cargo.lock -o flatpak/cargo-sources.json
cargo build --release -q
# 3. commit + tag + push
git add -A
git commit -q -m "Versão $VER

$NOTES"
git tag -a "v$VER" -m "v$VER"
git push -q origin HEAD "v$VER"
COMMIT=$(git rev-parse HEAD)
sed -i "s/^        tag: .*/        tag: v$VER/; s/^        commit: .*/        commit: $COMMIT/" "flatpak/$ID.yml"
git commit -q -am "flatpak: aponta para v$VER" && git push -q origin HEAD
# 4. Flathub
if [[ -d $FLATHUB/.git ]]; then
  cp "flatpak/$ID.yml" flatpak/cargo-sources.json "$FLATHUB/"
  git -C "$FLATHUB" add -A
  git -C "$FLATHUB" commit -q -m "Update to v$VER" || true
  git -C "$FLATHUB" push -q
  echo "✔ v$VER publicada no GitHub e enviada ao Flathub ($FLATHUB)"
else
  echo "✔ v$VER publicada no GitHub; clone do Flathub não encontrado em $FLATHUB"
fi

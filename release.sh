#!/usr/bin/env bash
# Publica uma versão: Cargo.toml + metainfo + tag no GitHub + repositório Flatpak
# em https://tevoetals.github.io/fastnotes (+ manifesto no Flathub, se houver clone).
#   ./release.sh 0.3.0 "O que mudou nesta versão"
# Quem instalou pelo Discover recebe a atualização na próxima verificação.
set -euo pipefail
cd "$(dirname "$0")"
ID=io.github.tevoetals.fastnotes
VER=${1:?versão, ex.: 0.3.0}
NOTES=${2:?descrição curta da versão}
FLATHUB=${FLATHUB_DIR:-$HOME/Programs/flathub-$ID}
[[ -z "$(git status --porcelain)" ]] || { echo "árvore suja: commit antes"; exit 1; }
git rev-parse -q --verify "refs/tags/v$VER" >/dev/null && { echo "a tag v$VER já existe: use outra versão"; exit 1; }
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
# 4. Repositório Flatpak próprio (GitHub Pages): o Discover instala e atualiza por ele.
#    Compila a partir da tag no GitHub, exatamente como o Flathub faria.
GPG=${FLATPAK_GPG_KEY:-DF1D9A48AE1AF10B2CC3B8FB525FB892E8422588}
gpg --list-secret-keys "$GPG" >/dev/null 2>&1 || GPG=""
flatpak run org.flatpak.Builder --user --force-clean --ccache --repo=flatpak/repo --default-branch=stable \
  ${GPG:+--gpg-sign=$GPG} flatpak/build "flatpak/$ID.yml" > /dev/null
flatpak build-update-repo --prune ${GPG:+--gpg-sign=$GPG} flatpak/repo > /dev/null
rm -rf flatpak/build .flatpak-builder/build
W=$(mktemp -d)
cp -r flatpak/site/. "$W/"
cp -r flatpak/repo "$W/repo"; rm -rf "$W/repo/tmp" "$W/repo/.lock"
git -C "$W" init -q -b gh-pages && git -C "$W" add -A && git -C "$W" commit -q -m "site: v$VER"
git -C "$W" push -q -f git@github.com:tevoetals/fastnotes.git gh-pages
rm -rf "$W"
echo "✔ v$VER publicada em https://tevoetals.github.io/fastnotes (Discover atualiza em ~1 h)"
# 5. Flathub: só quando o clone apontar para o repositório oficial flathub/$ID
#    (o fork da submissão é do usuário; não é atualizado automaticamente)
if [[ -d $FLATHUB/.git ]] && git -C "$FLATHUB" remote get-url origin | grep -q "flathub/$ID"; then
  cp "flatpak/$ID.yml" flatpak/cargo-sources.json "$FLATHUB/"
  git -C "$FLATHUB" add -A
  git -C "$FLATHUB" commit -q -m "Update to v$VER" || true
  git -C "$FLATHUB" push -q
  echo "✔ manifesto enviado ao Flathub ($FLATHUB)"
fi

#!/usr/bin/env bash
# Instala o Fast Notes em ~/.local/bin + entrada no menu/KRunner.
#
#   ./install.sh              binário pronto do GitHub, ou compila se houver Rust ≥ 1.85
#   ./install.sh --source     força compilar (otimizado para esta CPU)
#   ./install.sh --binary     força baixar o binário pronto
#   ./install.sh --uninstall  remove (as notas ficam)
#
# Funciona em Arch, Kubuntu/Ubuntu/Debian, Fedora etc. Não precisa de sudo.
# Pela loja (Discover), veja https://tevoetals.github.io/fastnotes
set -euo pipefail
cd "$(dirname "$0")"
ID=io.github.tevoetals.fastnotes
REPO=tevoetals/fastnotes
BIN="$HOME/.local/bin/fastnotes"
TG="$HOME/.local/bin/fastnotes-telegram"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
FONTS="${XDG_DATA_HOME:-$HOME/.local/share}/fonts/fastnotes"
RUST_MIN=1.85
MODE="${1:-auto}"

say() { printf '%s\n' "$*"; }
die() { printf 'erro: %s\n' "$*" >&2; exit 1; }

if [[ "$MODE" == "--uninstall" ]]; then
  if [[ -x "$TG" ]]; then "$TG" disable >/dev/null 2>&1 || true; fi
  rm -f "$BIN" "$TG" "$APPS/$ID.desktop" "$APPS/fastnotes.desktop" "$ICONS/$ID.svg"
  rm -rf "$FONTS"
  fc-cache -f >/dev/null 2>&1 || true
  update-desktop-database "$APPS" 2>/dev/null || true
  say "✔ removido (as notas em ~/.local/share/fastnotes foram mantidas)"
  exit 0
fi
[[ "$(uname -m)" == "x86_64" || "$MODE" == "--source" ]] || say "aviso: binário pronto só para x86_64; vou tentar compilar."

# Rust instalado pelo rustup fica em ~/.cargo/bin, que nem sempre está no PATH.
export PATH="$HOME/.cargo/bin:$PATH"
VER=""
[[ -f Cargo.toml ]] && VER=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

rust_ok() {
  command -v cargo >/dev/null 2>&1 || return 1
  local v; v=$(cargo --version 2>/dev/null | awk '{print $2}')
  [[ "$(printf '%s\n%s\n' "$RUST_MIN" "$v" | sort -V | head -1)" == "$RUST_MIN" ]]
}

# Para compilar: compilador C, pkg-config e o cabeçalho/link do libxkbcommon.
build_deps_ok() {
  local miss=()
  command -v cc >/dev/null 2>&1 || miss+=("compilador C")
  { ldconfig -p 2>/dev/null | grep -q 'libxkbcommon.so ' || [[ -e /usr/lib/x86_64-linux-gnu/libxkbcommon.so || -e /usr/lib/libxkbcommon.so || -e /usr/lib64/libxkbcommon.so ]]; } || miss+=("libxkbcommon (dev)")
  if ((${#miss[@]})); then
    say "faltam para compilar: ${miss[*]}"
    if command -v apt-get >/dev/null 2>&1; then say "  sudo apt install build-essential pkg-config libxkbcommon-dev"
    elif command -v dnf >/dev/null 2>&1; then say "  sudo dnf install gcc pkgconf libxkbcommon-devel"
    elif command -v pacman >/dev/null 2>&1; then say "  sudo pacman -S --needed base-devel libxkbcommon"; fi
    return 1
  fi
}

STAGE=""
cleanup() { if [[ -n "$STAGE" ]]; then rm -rf "$STAGE"; fi; }
trap cleanup EXIT

# Baixa o pacote pronto da versão (ou o mais recente) do GitHub.
fetch_binary() {
  local url tag="v$VER" name="fastnotes-x86_64-linux.tar.gz"
  STAGE=$(mktemp -d)
  for tag in "v$VER" latest; do
    if [[ "$tag" == latest ]]; then url="https://github.com/$REPO/releases/latest/download/$name"
    else url="https://github.com/$REPO/releases/download/$tag/$name"; fi
    say "baixando $url"
    if command -v curl >/dev/null 2>&1; then curl -fsSL "$url" -o "$STAGE/pkg.tgz" && break
    elif command -v wget >/dev/null 2>&1; then wget -q "$url" -O "$STAGE/pkg.tgz" && break
    else die "preciso de curl ou wget para baixar o binário"; fi
  done
  [[ -s "$STAGE/pkg.tgz" ]] || return 1
  tar xzf "$STAGE/pkg.tgz" -C "$STAGE" || return 1
  [[ -x "$STAGE/fastnotes/bin/fastnotes" ]] || return 1
  SRC_BIN="$STAGE/fastnotes/bin"
}

build_source() {
  build_deps_ok || return 1
  # Otimiza para esta CPU (o pacote pronto e o Flatpak usam o padrão portátil).
  RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}" cargo build --release || return 1
  SRC_BIN=target/release
}

SRC_BIN=""
if [[ -x bin/fastnotes && -x bin/fastnotes-telegram ]]; then
  SRC_BIN=bin   # rodando de dentro do pacote pronto
else
  case "$MODE" in
    --source) rust_ok || die "precisa de Rust ≥ $RUST_MIN (o do apt é antigo). Instale com: curl --proto '=https' -sSf https://sh.rustup.rs | sh"
              build_source || die "a compilação falhou" ;;
    --binary) fetch_binary || die "não consegui baixar o binário pronto" ;;
    *)
      if rust_ok && build_deps_ok >/dev/null; then
        build_source || { say "a compilação falhou; tentando o binário pronto"; fetch_binary || die "também não consegui baixar o binário pronto"; }
      else
        rust_ok || say "Rust ≥ $RUST_MIN não encontrado: usando o binário pronto."
        fetch_binary || die "não consegui baixar o binário pronto. Alternativas: instale o Rust (https://rustup.rs) e rode ./install.sh --source, ou instale pelo Flatpak (https://tevoetals.github.io/fastnotes)."
      fi ;;
  esac
fi
DATA_DIR=.
[[ -n "$STAGE" && "$SRC_BIN" == "$STAGE"/* ]] && DATA_DIR="$STAGE/fastnotes"

install -Dm755 "$SRC_BIN/fastnotes" "$BIN"
install -Dm755 "$SRC_BIN/fastnotes-telegram" "$TG"
# Serviço do Telegram já ligado: reinicia com o binário novo.
systemctl --user try-restart fastnotes-telegram.service 2>/dev/null || true
# Fonte Inter (OFL) — usada pelo app e disponível ao sistema.
install -Dm644 "$DATA_DIR"/fonts/InterVariable.ttf "$DATA_DIR"/fonts/InterVariable-Italic.ttf "$DATA_DIR"/fonts/LICENSE-Inter.txt -t "$FONTS"
fc-cache -f "$FONTS" >/dev/null 2>&1 || true
install -Dm644 "$DATA_DIR/data/icons/$ID.svg" -t "$ICONS"
rm -f "$APPS/fastnotes.desktop"   # nome antigo
mkdir -p "$APPS"
sed "s|^Exec=.*|Exec=$BIN|" "$DATA_DIR/data/$ID.desktop" > "$APPS/$ID.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
# KDE: atualiza o cache do menu/KRunner na hora.
for k in kbuildsycoca6 kbuildsycoca5; do command -v "$k" >/dev/null 2>&1 && { "$k" >/dev/null 2>&1 || true; break; }; done

say "✔ instalado: $BIN"
say "  fonte Inter em: $FONTS"
say "  notas em:  ${FASTNOTES_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/fastnotes/notes}"
say "  atalho global: Configurações do Sistema → Atalhos → Adicionar novo → Comando: fastnotes"

# Avisos sobre o que falta para rodar.
if ! { ldconfig -p 2>/dev/null | grep -q 'libxkbcommon.so.0'; }; then
  say "⚠ falta a biblioteca libxkbcommon (teclado). Kubuntu/Ubuntu: sudo apt install libxkbcommon0"
fi
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) say "⚠ ~/.local/bin não está no PATH deste terminal; o menu funciona, mas para digitar 'fastnotes' faça logout/login." ;; esac
if [[ "${XDG_SESSION_TYPE:-}" == "x11" ]]; then
  say "⚠ esta sessão é X11 e o Fast Notes precisa de Wayland."
  say "  Saia da sessão e, na tela de login, escolha \"Plasma (Wayland)\"."
fi
exit 0

#!/usr/bin/env bash
# Compila em release e instala em ~/.local/bin + entrada no menu/KRunner.
# Desinstalar: ./install.sh --uninstall
# (Para instalar pela loja, use o Flatpak: flatpak install flathub io.github.tevoetals.fastnotes)
set -euo pipefail
cd "$(dirname "$0")"
ID=io.github.tevoetals.fastnotes
BIN="$HOME/.local/bin/fastnotes"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/scalable/apps"
FONTS="${XDG_DATA_HOME:-$HOME/.local/share}/fonts/fastnotes"
if [[ "${1:-}" == "--uninstall" ]]; then
  rm -f "$BIN" "$APPS/$ID.desktop" "$APPS/fastnotes.desktop" "$ICONS/$ID.svg"
  rm -rf "$FONTS"
  fc-cache -f >/dev/null 2>&1 || true
  update-desktop-database "$APPS" 2>/dev/null || true
  echo "✔ removido (as notas em ~/.local/share/fastnotes foram mantidas)"
  exit 0
fi
# Otimiza para esta CPU (o build do Flatpak usa o padrão portátil).
RUSTFLAGS="${RUSTFLAGS:--C target-cpu=native}" cargo build --release
install -Dm755 target/release/fastnotes "$BIN"
# Fonte Inter (OFL) — usada pelo app e disponível ao sistema.
install -Dm644 fonts/InterVariable.ttf fonts/InterVariable-Italic.ttf fonts/LICENSE-Inter.txt -t "$FONTS"
fc-cache -f "$FONTS" >/dev/null 2>&1 || true
install -Dm644 "data/icons/$ID.svg" -t "$ICONS"
rm -f "$APPS/fastnotes.desktop"   # nome antigo
sed "s|^Exec=.*|Exec=$BIN|" "data/$ID.desktop" > "$APPS/$ID.desktop"
update-desktop-database "$APPS" 2>/dev/null || true
echo "✔ instalado: $BIN"
echo "  fonte Inter em: $FONTS"
echo "  notas em:  ${FASTNOTES_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/fastnotes/notes}"
echo "  atalho global: Configurações do Sistema → Atalhos → Adicionar novo → Comando: fastnotes"

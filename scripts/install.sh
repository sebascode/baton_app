#!/usr/bin/env bash
# Compila baton en release y lo instala (o actualiza) en esta máquina.
#
#   scripts/install.sh              compila e instala en ~/.local/bin
#   scripts/install.sh --dir RUTA   instala en otra carpeta
#   scripts/install.sh --rollback   vuelve a la versión instalada antes de la última
#
# También instala la página de manual (man baton) en ~/.local/share/man (o BATON_MAN_DIR).
#
# Antes de reemplazar el binario deja una copia como `baton.prev`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${BATON_INSTALL_DIR:-$HOME/.local/bin}"
rollback=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dir)
      [ $# -ge 2 ] || { echo "error: --dir necesita una carpeta" >&2; exit 2; }
      dest="$2"; shift 2 ;;
    --rollback) rollback=1; shift ;;
    -h|--help) sed -n '2,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: opción desconocida '$1' (usa --help)" >&2; exit 2 ;;
  esac
done

mkdir -p "$dest"

# Se copia a un archivo temporal de la misma carpeta y se renombra: reemplazar así es atómico y
# funciona aunque baton esté corriendo en ese momento.
place() {
  local src="$1" tmp
  tmp="$(mktemp "$dest/.baton.XXXXXX")"
  cp "$src" "$tmp"
  chmod 755 "$tmp"
  mv -f "$tmp" "$dest/baton"
}

if [ "$rollback" = 1 ]; then
  [ -f "$dest/baton.prev" ] || { echo "error: no hay versión anterior en $dest/baton.prev" >&2; exit 1; }
  place "$dest/baton.prev"
  echo "versión anterior restaurada en $dest/baton"
else
  command -v cargo >/dev/null || { echo "error: no se encontró cargo" >&2; exit 1; }
  echo "compilando (release)..."
  (cd "$root" && cargo build --release --locked -p baton)
  if [ -x "$dest/baton" ]; then
    cp -f "$dest/baton" "$dest/baton.prev"
  fi
  place "$root/target/release/baton"
  echo "instalado en $dest/baton"
fi

# La página de manual (`man baton`). `~/.local/share/man` ya está en el MANPATH de man-db.
install_man() {
  local man_dir="${BATON_MAN_DIR:-$HOME/.local/share/man}/man1"
  if [ -f "$root/man/baton.1" ] && mkdir -p "$man_dir" 2>/dev/null; then
    cp "$root/man/baton.1" "$man_dir/baton.1" && chmod 644 "$man_dir/baton.1"
    echo "manual instalado en $man_dir/baton.1 (man baton)"
  fi
}
if [ "$rollback" = 0 ]; then
  install_man
fi

"$dest/baton" version

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "aviso: $dest no está en el PATH" >&2 ;;
esac

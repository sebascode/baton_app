#!/usr/bin/env bash
# Actualiza la fórmula de Homebrew (repo sebascode/homebrew-baton) a una versión ya publicada.
#
#   scripts/release-tap.sh              usa la versión del Cargo.toml
#   scripts/release-tap.sh 0.3.0        o la que se indique
#   scripts/release-tap.sh --dry-run    muestra el cambio sin hacer commit ni push
#
# Pasos de un release: subir la versión en Cargo.toml, commit y push, `git tag -a vX.Y.Z` y
# `git push origin vX.Y.Z` (el workflow publica los binarios) y, al final, este script.
#
# Variables: BATON_TAP_REPO (por defecto git@github.com:sebascode/homebrew-baton.git) y
# BATON_APP_URL (por defecto https://github.com/sebascode/baton_app).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tap_repo="${BATON_TAP_REPO:-git@github.com:sebascode/homebrew-baton.git}"
app_url="${BATON_APP_URL:-https://github.com/sebascode/baton_app}"
dry=0
version=""

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) dry=1; shift ;;
    -h|--help) sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "opción desconocida '$1' (usa --help)" ;;
    *) [ -z "$version" ] || die "solo se acepta una versión"; version="$1"; shift ;;
  esac
done

if [ -z "$version" ]; then
  version="$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
  [ -n "$version" ] || die "no se pudo leer la versión de Cargo.toml"
fi
version="${version#v}"
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "la versión '$version' no es X.Y.Z" ;;
esac
case "$version" in
  *[!0-9.]*) die "la versión '$version' no es X.Y.Z" ;;
esac

command -v curl >/dev/null || die "hace falta curl"
command -v git >/dev/null || die "hace falta git"
if command -v sha256sum >/dev/null; then
  sha_of() { sha256sum | cut -d' ' -f1; }
elif command -v shasum >/dev/null; then
  sha_of() { shasum -a 256 | cut -d' ' -f1; }
else
  die "hace falta sha256sum o shasum"
fi

tag="v$version"
archive="$app_url/archive/refs/tags/$tag.tar.gz"
echo "calculando el sha256 de $archive ..."
sha="$(curl -fsSL "$archive" | sha_of)" ||
  die "no se pudo bajar $archive (¿existe el tag $tag y está subido? git push origin $tag)"
[ "${#sha}" = 64 ] || die "sha256 inesperado: '$sha'"
echo "sha256: $sha"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
git clone -q "$tap_repo" "$work/tap" || die "no se pudo clonar $tap_repo"
formula="$work/tap/Formula/baton.rb"
[ -f "$formula" ] || die "el tap no tiene Formula/baton.rb"

new_url="$app_url/archive/refs/tags/$tag.tar.gz"
current_url="$(sed -n 's/^  url "\(.*\)"$/\1/p' "$formula")"
current_sha="$(sed -n 's/^  sha256 "\(.*\)"$/\1/p' "$formula")"
if [ "$current_url" = "$new_url" ] && [ "$current_sha" = "$sha" ]; then
  echo "la fórmula ya está en $version: no hay nada que hacer"
  exit 0
fi

# solo se tocan las líneas `url` y `sha256` del paquete (las dos primeras de cada una)
tmp="$work/baton.rb"
awk -v url="$new_url" -v sha="$sha" '
  !u && /^  url "/    { print "  url \"" url "\""; u = 1; next }
  !s && /^  sha256 "/ { print "  sha256 \"" sha "\""; s = 1; next }
  { print }
' "$formula" > "$tmp"
grep -q "^  url \"$new_url\"$" "$tmp" && grep -q "^  sha256 \"$sha\"$" "$tmp" ||
  die "no se pudo editar la fórmula (¿cambió su formato?)"
cp "$tmp" "$formula"

echo
git -C "$work/tap" --no-pager diff -- Formula/baton.rb | sed 's/^/  /'
if [ "$dry" = 1 ]; then
  echo
  echo "(--dry-run: no se hizo commit ni push)"
  exit 0
fi

git -C "$work/tap" add Formula/baton.rb
git -C "$work/tap" commit -q -m "baton $version"
git -C "$work/tap" push -q origin HEAD || die "no se pudo subir el cambio al tap"
echo
echo "tap actualizado a $version. En cada máquina: brew update && brew upgrade baton"

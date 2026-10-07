#!/usr/bin/env bash
# Arma los paquetes .deb y .rpm de baton (amd64 y arm64) con nfpm, a partir de los binarios de
# Linux de un release (los mismos que baja `baton update`, estáticos con musl).
#
#   scripts/build-packages.sh VERSION CARPETA
#
# CARPETA ya trae baton-vVERSION-linux-x86_64.tar.gz y baton-vVERSION-linux-aarch64.tar.gz (es donde
# los deja el workflow de release). Los paquetes quedan en la misma carpeta, cada uno con su
# .sha256. nfpm se baja con una versión y una suma fijas.
set -euo pipefail

version="${1:?uso: build-packages.sh VERSION CARPETA}"
out="$(cd "${2:?uso: build-packages.sh VERSION CARPETA}" && pwd)"
version="${version#v}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

NFPM_VERSION="2.47.0"
declare -A NFPM_SHA=(
  [x86_64]="0660ca602b2d2d2ae4781a06c692b3eeb9d437ffea05b831d76e41f4a3188783"
  [arm64]="1c0f5f2999b9a974bfb04fdb0cc3306096de530ac5dbb25d739cc5f5219c919c"
)

case "$(uname -m)" in
  x86_64) host=x86_64 ;;
  aarch64 | arm64) host=arm64 ;;
  *) echo "error: arquitectura no soportada para correr nfpm: $(uname -m)" >&2; exit 1 ;;
esac

sha_of() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "bajando nfpm $NFPM_VERSION ($host) ..."
tarball="nfpm_${NFPM_VERSION}_Linux_${host}.tar.gz"
curl -fsSL -o "$work/$tarball" \
  "https://github.com/goreleaser/nfpm/releases/download/v${NFPM_VERSION}/$tarball"
[ "$(sha_of "$work/$tarball")" = "${NFPM_SHA[$host]}" ] ||
  { echo "error: la suma de nfpm no coincide: no se usa" >&2; exit 1; }
tar -xzf "$work/$tarball" -C "$work" nfpm

# arquitectura del release -> arquitectura de nfpm (amd64 | arm64)
for pair in "x86_64:amd64" "aarch64:arm64"; do
  platform="linux-${pair%%:*}"
  arch="${pair##*:}"
  src="$out/baton-v$version-$platform.tar.gz"
  [ -f "$src" ] || { echo "error: falta $src" >&2; exit 1; }
  stage="$work/$arch"
  mkdir -p "$stage"
  tar -xzf "$src" -C "$stage" baton baton.1
  gzip -9n "$stage/baton.1"
  for format in deb rpm; do
    ARCH="$arch" VERSION="$version" BINARY="$stage/baton" MANPAGE="$stage/baton.1.gz" \
      "$work/nfpm" package --config "$root/packaging/nfpm.yaml" --packager "$format" \
      --target "$out/" >/dev/null
  done
done

for pkg in "$out"/*.deb "$out"/*.rpm; do
  echo "$(sha_of "$pkg")  $(basename "$pkg")" > "$pkg.sha256"
  echo "listo: $(basename "$pkg")"
done

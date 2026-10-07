#!/bin/sh
# Instala baton desde los binarios de GitHub Releases: sin Rust y sin Homebrew.
#
#   curl -fsSL https://raw.githubusercontent.com/sebascode/baton_app/main/scripts/get.sh | sh
#   curl -fsSL .../get.sh | sh -s -- --version 0.3.0 --dir /usr/local/bin
#
#   --version X.Y.Z   instala esa versión (por defecto, la última)
#   --dir RUTA        carpeta donde queda `baton` (por defecto ~/.local/bin)
#
# Baja el archivo de tu plataforma (Linux x86_64 o aarch64, macOS con Apple Silicon), comprueba su
# suma sha256, comprueba que el binario responda con la versión esperada y recién entonces lo
# instala, de forma atómica. Si ya había uno lo deja como `baton.prev`. También instala la página de
# manual en ~/.local/share/man (o BATON_MAN_DIR). Después se actualiza con `baton update`.
#
# Variables: BATON_VERSION, BATON_INSTALL_DIR, BATON_MAN_DIR y BATON_REPO_URL.
set -eu

repo="${BATON_REPO_URL:-https://github.com/sebascode/baton_app}"
dest="${BATON_INSTALL_DIR:-$HOME/.local/bin}"
version="${BATON_VERSION:-}"

die() {
  echo "error: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version necesita un valor"
      version="$2"
      shift 2
      ;;
    --dir)
      [ $# -ge 2 ] || die "--dir necesita una carpeta"
      dest="$2"
      shift 2
      ;;
    -h | --help)
      cat <<'AYUDA'
Instala baton desde los binarios de GitHub Releases (sin Rust ni Homebrew).

  curl -fsSL https://raw.githubusercontent.com/sebascode/baton_app/main/scripts/get.sh | sh
  curl -fsSL .../get.sh | sh -s -- --version 0.3.0 --dir /usr/local/bin

  --version X.Y.Z   instala esa versión (por defecto, la última)
  --dir RUTA        carpeta donde queda baton (por defecto ~/.local/bin)

Comprueba la suma sha256 y que el binario responda con la versión esperada antes de instalar.
Variables: BATON_VERSION, BATON_INSTALL_DIR, BATON_MAN_DIR y BATON_REPO_URL.
AYUDA
      exit 0
      ;;
    *) die "opción desconocida '$1' (usa --help)" ;;
  esac
done

command -v curl >/dev/null 2>&1 || die "hace falta curl"
command -v tar >/dev/null 2>&1 || die "hace falta tar"
if command -v sha256sum >/dev/null 2>&1; then
  sha_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
  sha_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
  die "hace falta sha256sum o shasum para verificar la descarga"
fi

# La plataforma, con los nombres de los archivos del release.
os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Linux/x86_64) platform="linux-x86_64" ;;
  Linux/aarch64 | Linux/arm64) platform="linux-aarch64" ;;
  Darwin/arm64) platform="macos-aarch64" ;;
  Darwin/x86_64) die "no se publican binarios para Mac con Intel; usa 'brew install sebascode/baton/baton' o compila con scripts/install.sh" ;;
  *) die "no se publican binarios para $os $arch" ;;
esac

# La última versión: GitHub redirige releases/latest a releases/tag/vX.Y.Z (no baja nada).
if [ -z "$version" ]; then
  url="$(curl -fsS --proto '=https' --connect-timeout 15 -m 30 -o /dev/null -w '%{redirect_url}' "$repo/releases/latest")" ||
    die "no se pudo consultar la última versión en GitHub"
  case "$url" in
    */releases/tag/*) version="${url##*/releases/tag/}" ;;
    *) die "no hay ningún release publicado todavía" ;;
  esac
fi
version="${version#v}"
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "la versión '$version' no es X.Y.Z" ;;
esac
case "$version" in
  *[!0-9.]*) die "la versión '$version' no es X.Y.Z" ;;
esac

asset="baton-v$version-$platform.tar.gz"
base="$repo/releases/download/v$version"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "descargando $asset ..."
fetch() {
  curl -fsSL --proto '=https' --proto-redir '=https' --connect-timeout 15 -m 300 -o "$2" "$1" ||
    die "no se pudo bajar $1 (¿existe la versión $version para $platform?)"
}
fetch "$base/$asset" "$tmp/$asset"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256"

expected="$(cut -d' ' -f1 "$tmp/$asset.sha256" | head -n 1)"
case "$expected" in
  *[!0-9a-fA-F]* | "") die "el archivo .sha256 del release no trae un hash válido" ;;
esac
[ "${#expected}" -eq 64 ] || die "el archivo .sha256 del release no trae un hash válido"
actual="$(sha_of "$tmp/$asset")"
[ "$actual" = "$expected" ] ||
  die "la suma de verificación no coincide (esperada $expected, obtenida $actual): no se instaló nada"
echo "suma sha256 verificada"

mkdir "$tmp/x"
tar -xzf "$tmp/$asset" -C "$tmp/x" || die "no se pudo extraer $asset"
[ -f "$tmp/x/baton" ] || die "el archivo descargado no trae el binario baton"
chmod 755 "$tmp/x/baton"
said="$("$tmp/x/baton" version 2>/dev/null || true)"
case "$said" in
  "baton $version"*) ;;
  *) die "el binario descargado no responde como baton $version (dijo: '$said'): no se instaló nada" ;;
esac

mkdir -p "$dest" 2>/dev/null || die "no se pudo crear $dest"
[ -w "$dest" ] || die "no se puede escribir en $dest (usa --dir con una carpeta tuya, o sudo)"
previous=""
if [ -x "$dest/baton" ]; then
  previous="$("$dest/baton" version 2>/dev/null | head -n 1 || true)"
  # rm antes de copiar: en macOS sobrescribir un binario en su sitio puede invalidar su firma
  rm -f "$dest/baton.prev"
  cp "$dest/baton" "$dest/baton.prev"
fi
# a un archivo temporal de la misma carpeta y se renombra: es atómico y funciona aunque baton corra
staged="$dest/.baton.$$"
cp "$tmp/x/baton" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$dest/baton"
if [ -n "$previous" ]; then
  echo "actualizado en $dest/baton (antes: $previous)"
else
  echo "instalado en $dest/baton"
fi

# la página de manual (`man baton`)
if [ -f "$tmp/x/baton.1" ]; then
  man_base="${BATON_MAN_DIR:-$HOME/.local/share/man}"
  if mkdir -p "$man_base/man1" 2>/dev/null && cp "$tmp/x/baton.1" "$man_base/man1/baton.1" 2>/dev/null; then
    chmod 644 "$man_base/man1/baton.1"
    echo "manual instalado en $man_base/man1/baton.1 (man baton)"
  fi
fi

"$dest/baton" version

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "aviso: $dest no está en el PATH; agrégalo a tu shell: export PATH=\"$dest:\$PATH\"" >&2 ;;
esac
echo "para actualizar más adelante: baton update"

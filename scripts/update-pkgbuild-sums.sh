#!/usr/bin/env bash
# Compute real SHA-256 checksums for the release source tarball and desktop entry,
# backfill `sha256sums=(...)` in `packaging/arch/PKGBUILD` (no `'SKIP'`), and
# regenerate `packaging/arch/.SRCINFO`.
#
# Usage:
#   bash scripts/update-pkgbuild-sums.sh [<version-or-tag>] [--tarball <local-archive.tar.gz>]

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH_DIR="${ROOT}/packaging/arch"
PKGBUILD_FILE="${ARCH_DIR}/PKGBUILD"
DESKTOP_FILE="${ARCH_DIR}/omnidoppelganger.desktop"
SRCINFO_FILE="${ARCH_DIR}/.SRCINFO"

VERSION=""
LOCAL_TARBALL=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      echo "Usage: bash scripts/update-pkgbuild-sums.sh [<version-or-tag>] [--tarball <local-archive.tar.gz>]"
      exit 0
      ;;
    --tarball)
      if [[ $# -lt 2 ]]; then
        echo "ERROR: --tarball requires a file path" >&2
        exit 1
      fi
      LOCAL_TARBALL="$2"
      shift 2
      ;;
    --tarball=*)
      LOCAL_TARBALL="${1#*=}"
      shift
      ;;
    -*)
      echo "ERROR: unknown option '$1'" >&2
      exit 1
      ;;
    *)
      if [[ -n "$VERSION" ]]; then
        echo "ERROR: unexpected extra argument '$1'" >&2
        exit 1
      fi
      VERSION="${1#v}"
      VERSION="${VERSION#V}"
      shift
      ;;
  esac
done

sha256_file() {
  local target="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$target" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$target" | awk '{print tolower($1)}'
  else
    echo "ERROR: neither sha256sum nor shasum is installed" >&2
    exit 1
  fi
}

if [[ ! -f "$PKGBUILD_FILE" ]]; then
  echo "ERROR: missing ${PKGBUILD_FILE}" >&2
  exit 1
fi
if [[ ! -f "$DESKTOP_FILE" ]]; then
  echo "ERROR: missing ${DESKTOP_FILE}" >&2
  exit 1
fi

if [[ -n "$VERSION" ]]; then
  PKGVER_SAFE="${VERSION//-/_}"
  TMP_PKG="$(mktemp "${TMPDIR:-/tmp}/odg-pkgbuild-XXXXXX")"
  sed "s/^pkgver=.*/pkgver=${PKGVER_SAFE}/" "$PKGBUILD_FILE" > "$TMP_PKG"
  mv "$TMP_PKG" "$PKGBUILD_FILE"
else
  VERSION="$(awk -F= '/^pkgver=/{print $2; exit}' "$PKGBUILD_FILE" | tr -d "\"' ")"
fi

URL="$(awk -F= '/^url=/{print $2; exit}' "$PKGBUILD_FILE" | tr -d "\"'")"
PKGNAME="$(awk -F= '/^pkgname=/{print $2; exit}' "$PKGBUILD_FILE" | tr -d "\"' ")"
PKGREL="$(awk -F= '/^pkgrel=/{print $2; exit}' "$PKGBUILD_FILE" | tr -d "\"' ")"
PKGDESC="$(sed -n 's/^pkgdesc="\(.*\)"$/\1/p' "$PKGBUILD_FILE" | head -n 1)"

TMP_DL=""
cleanup() {
  if [[ -n "$TMP_DL" && -f "$TMP_DL" ]]; then
    rm -f "$TMP_DL"
  fi
}
trap cleanup EXIT

if [[ -n "$LOCAL_TARBALL" ]]; then
  if [[ ! -f "$LOCAL_TARBALL" ]]; then
    echo "ERROR: tarball not found at '${LOCAL_TARBALL}'" >&2
    exit 1
  fi
  TARBALL_SHA="$(sha256_file "$LOCAL_TARBALL")"
else
  ARCHIVE_URL="${URL}/archive/refs/tags/v${VERSION}.tar.gz"
  TMP_DL="$(mktemp "${TMPDIR:-/tmp}/odg-release-tar-XXXXXX")"
  echo "Downloading ${ARCHIVE_URL} ..."
  curl -fsSL --retry 3 "$ARCHIVE_URL" -o "$TMP_DL"
  TARBALL_SHA="$(sha256_file "$TMP_DL")"
fi

DESKTOP_SHA="$(sha256_file "$DESKTOP_FILE")"

for sum in "$TARBALL_SHA" "$DESKTOP_SHA"; do
  if [[ ! "$sum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "ERROR: invalid SHA-256 digest '${sum}'" >&2
    exit 1
  fi
done

TMP_PKG="$(mktemp "${TMPDIR:-/tmp}/odg-pkgbuild-sums-XXXXXX")"
sed "s/^sha256sums=(.*/sha256sums=('${TARBALL_SHA}' '${DESKTOP_SHA}')/" "$PKGBUILD_FILE" > "$TMP_PKG"
mv "$TMP_PKG" "$PKGBUILD_FILE"

if command -v makepkg >/dev/null 2>&1; then
  (
    cd "$ARCH_DIR"
    makepkg --printsrcinfo > "$SRCINFO_FILE"
  )
else
  cat > "$SRCINFO_FILE" <<EOF
pkgbase = ${PKGNAME}
	pkgdesc = ${PKGDESC}
	pkgver = ${VERSION}
	pkgrel = ${PKGREL}
	url = ${URL}
	arch = x86_64
	arch = aarch64
	license = MIT
	makedepends = cargo
	makedepends = git
	makedepends = nodejs
	makedepends = pkgconf
	makedepends = pnpm
	makedepends = rust
	depends = cairo
	depends = gdk-pixbuf2
	depends = glib2
	depends = gtk3
	depends = hicolor-icon-theme
	depends = libayatana-appindicator
	depends = openssl
	depends = webkit2gtk-4.1
	optdepends = git: worktree isolation and Git panel support
	optdepends = github-cli: one-click GitHub pull request creation
	options = !lto
	source = ${PKGNAME}-${VERSION}.tar.gz::${URL}/archive/refs/tags/v${VERSION}.tar.gz
	source = omnidoppelganger.desktop
	sha256sums = ${TARBALL_SHA}
	sha256sums = ${DESKTOP_SHA}

pkgname = ${PKGNAME}
EOF
fi

echo "Updated ${PKGBUILD_FILE} and ${SRCINFO_FILE} for v${VERSION}:"
echo "  source tarball sha256 : ${TARBALL_SHA}"
echo "  desktop entry sha256  : ${DESKTOP_SHA}"

#!/usr/bin/env bash
# Build an Arch Linux .pkg.tar.zst package directly from the current repository tree.
# Run on an Arch Linux host (or inside an archlinux container) as a non-root user.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
VERSION="$(node -p "require('${ROOT}/package.json').version")"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/odg-arch-XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK"
cp "${ROOT}/packaging/arch/omnidoppelganger.desktop" "$WORK/"
sed \
  -e "s/^pkgver=.*/pkgver=${VERSION}/" \
  -e "s|source=(.*|source=(\"omnidoppelganger-${VERSION}.tar.gz\" \"omnidoppelganger.desktop\")|" \
  "${ROOT}/packaging/arch/PKGBUILD" > "$WORK/PKGBUILD"

git -C "$ROOT" archive --format=tar.gz --prefix="omnidoppelganger-${VERSION}/" -o "$WORK/omnidoppelganger-${VERSION}.tar.gz" HEAD

(
  cd "$WORK"
  makepkg -f --noconfirm
)

OUT="${ROOT}/target/arch"
mkdir -p "$OUT"
cp "$WORK"/*.pkg.tar.* "$OUT/"
echo "Built Arch Linux package(s) in ${OUT}:"
ls -lh "$OUT"/*.pkg.tar.*

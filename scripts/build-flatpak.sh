#!/usr/bin/env bash
# Builds the Flatpak (flatpak/io.vectra.rustypainter.yml), installs it for
# this user and writes target/rusty-painter-<version>.flatpak to hand out:
#   flatpak install --user rusty-painter-<version>.flatpak
# Needs, once: flatpak install --user flathub org.flatpak.Builder \
#   org.freedesktop.Sdk//25.08 org.freedesktop.Sdk.Extension.rust-stable//25.08
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
flatpak run org.flatpak.Builder --user --install --force-clean \
  --state-dir=target/flatpak-state --repo=target/flatpak-repo \
  target/flatpak flatpak/io.vectra.rustypainter.yml
flatpak build-bundle --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
  target/flatpak-repo "target/rusty-painter-${VERSION}.flatpak" io.vectra.rustypainter
echo "Built target/rusty-painter-${VERSION}.flatpak"

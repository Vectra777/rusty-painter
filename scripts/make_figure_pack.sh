#!/usr/bin/env bash
# Rebuild the realistic figures (assets/figure-pack/v1) from MakeHuman's CC0
# assets: fetches MPFB 2 (the MakeHuman add-on for Blender, whose assets are
# CC0 1.0 and code GPL-3.0), installs it into a throwaway Blender profile
# (your own Blender settings are left alone) and runs
# scripts/make_figure_pack.py. Needs blender (4.2 or later), git and zip.
#
#   scripts/make_figure_pack.sh [MPFB_COMMIT]
#
# MPFB_COMMIT defaults to the one the shipped pack was built from.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMMIT="${1:-9de0707fdc88438d7c53a1a2ac7937d9221e2431}"
BLENDER="${BLENDER:-blender}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

git clone -q https://github.com/makehumancommunity/mpfb2.git "$WORK/mpfb2"
git -C "$WORK/mpfb2" checkout -q "$COMMIT"
(cd "$WORK/mpfb2/src" && zip -qr "$WORK/mpfb.zip" mpfb -x '*__pycache__*')

export BLENDER_USER_RESOURCES="$WORK/blender/res"
export BLENDER_USER_CONFIG="$WORK/blender/config"
export BLENDER_USER_SCRIPTS="$WORK/blender/scripts"
export BLENDER_USER_EXTENSIONS="$WORK/blender/extensions"
export BLENDER_USER_DATAFILES="$WORK/blender/datafiles"
"$BLENDER" -b --factory-startup --command extension install-file -r user_default -e "$WORK/mpfb.zip"

MPFB_COMMIT="$COMMIT" "$BLENDER" -b --python "$ROOT/scripts/make_figure_pack.py" -- \
  "$ROOT/assets/figure-pack/v1"
echo "Done: assets/figure-pack/v1 (its manifest's content stamp is updated)."

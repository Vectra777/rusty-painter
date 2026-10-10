#!/usr/bin/env bash
# Build the iPad app on macOS, without an Xcode project: the bundle is the
# binary, ios/Info.plist and the privacy manifest (everything else is built
# into the binary).
#
#   scripts/build-ios.sh [target] [debug|release]
#
# target: aarch64-apple-ios (an iPad; the default) or aarch64-apple-ios-sim
# (the Simulator, which gets only the .app).
#
# iPad builds give target/<target>/<mode>/RustyPainter.ipa, signed for the
# App Store (TestFlight) when IOS_SIGN_IDENTITY (a keychain identity, e.g.
# "Apple Distribution: …") and IOS_PROFILE (its .mobileprovision) are set,
# and otherwise ad hoc: for SideStore, which signs it again with your own
# Apple ID on the iPad.
set -euo pipefail

# Rust and the C dependencies (built by `cc`) for the same iOS as
# Info.plist's MinimumOSVersion: left alone, Rust links for iOS 10 and `cc`
# builds for the SDK's own (newer) version, and the link fails.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

TARGET="${1:-aarch64-apple-ios}"
MODE="${2:-release}"

if [[ "${MODE}" == release ]]; then
  cargo build --locked --bin rusty-painter --target "${TARGET}" --release
else
  cargo build --locked --bin rusty-painter --target "${TARGET}"
fi

OUT="target/${TARGET}/${MODE}"
APP="${OUT}/RustyPainter.app"
rm -rf "${APP}"
mkdir -p "${APP}"
cp "${OUT}/rusty-painter" "${APP}/"
cp ios/Info.plist ios/PrivacyInfo.xcprivacy "${APP}/"

VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
plutil -replace CFBundleShortVersionString -string "${VERSION}" "${APP}/Info.plist"
# TestFlight wants a new build number for each upload of a version.
plutil -replace CFBundleVersion -string "${IOS_BUILD_NUMBER:-${VERSION}}" "${APP}/Info.plist"

if [[ "${TARGET}" == *-sim ]]; then
  plutil -replace CFBundleSupportedPlatforms -json '["iPhoneSimulator"]' "${APP}/Info.plist"
  codesign --force --sign - "${APP}"
  echo "${APP}"
  exit 0
fi

if [[ -n "${IOS_SIGN_IDENTITY:-}" ]]; then
  cp "${IOS_PROFILE}" "${APP}/embedded.mobileprovision"
  security cms -D -i "${IOS_PROFILE}" > "${OUT}/profile.plist"
  plutil -extract Entitlements xml1 -o "${OUT}/entitlements.plist" "${OUT}/profile.plist"
  codesign --force --sign "${IOS_SIGN_IDENTITY}" \
    --entitlements "${OUT}/entitlements.plist" "${APP}"
else
  codesign --force --sign - "${APP}"
fi

rm -rf "${OUT}/Payload" "${OUT}/RustyPainter.ipa"
mkdir "${OUT}/Payload"
cp -R "${APP}" "${OUT}/Payload/"
(cd "${OUT}" && zip -qr RustyPainter.ipa Payload)
echo "${OUT}/RustyPainter.ipa"

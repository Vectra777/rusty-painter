# Building

## Desktop

With a Rust toolchain (`cargo`, `rustc`), stable 1.88 or newer:

```bash
cargo run --release
```

### Windows
Build with the MSVC Rust toolchain (Visual Studio's C++ build tools and the Windows SDK); `./scripts/check-windows.ps1` runs the same checks as CI. Release builds have no console, so the app logs to `rusty-painter.log` in the [data folder](../user/install.md#where-your-data-is-kept) (errors at start-up, the GPU it picked, panics with their backtrace). See [Debugging on Windows](windows-debugging.md) for what to try when it doesn't start or the pen misbehaves.

### Flatpak
`scripts/build-flatpak.sh` builds the Flatpak from `flatpak/io.vectra.rustypainter.yml` (with a slim ffmpeg for video export). The release workflow builds it too.

### Icons
`scripts/make-icons.sh` makes every platform's icon (window, `.exe`, Android's adaptive icon, iPad) from one picture.

### README pictures
The demo GIF and the screenshot come from the app painting a scene by itself (`RP_DEMO`, in `src/app/dev_shot.rs`), the brush showcase from an example:

```bash
mkdir -p /tmp/demo
RUSTY_PAINTER_DATA=/tmp/demo-data RP_SIZE=1600x1000 RP_OPEN=brush,colour,layers RP_DEMO=/tmp/demo cargo run --release
cp "$(ls /tmp/demo/*.png | tail -1)" docs/src/screenshot.png
ffmpeg -framerate 15 -i /tmp/demo/%04d.png -vf "tpad=stop_mode=clone:stop_duration=2,scale=800:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=192[p];[b][p]paletteuse=dither=bayer:bayer_scale=4" docs/src/demo.gif
cargo run --release --features bench --example brush_showcase -- docs/src/brushes.png
```
`RUSTY_PAINTER_DATA` keeps your own settings and autosave out of it.

## Android (APK)
Building for Android needs more than the target and `cargo-apk`: this repo vendors and patches `winit` and ships its own NDK linker wrappers, both required for the build to work:

- `vendor-winit/`: a patched copy of `winit`, pulled in via `[patch.crates-io]` in `Cargo.toml`, needed for Android windowing/lifecycle support beyond what upstream `winit` provides out of the box.
- `scripts/linkers/` + `scripts/build-android.sh`: wrapper scripts that route each Android target's linker invocation through the NDK toolchain; `.cargo/config.toml` points cargo at them. Run `scripts/build-android.sh` rather than a bare `cargo apk build` to make sure these are picked up.
- `src/android.rs`: the native JNI/lifecycle glue (`android_logger`, `jni`, `ndk-context`) that makes the app actually run once launched, as opposed to just compiling for the target.
- `android/java/` + `android/classes.dex`: two small Java helpers the NativeActivity can't do without: the system file picker (open/save documents, brushes, palettes) and the soft keyboard. `cargo-apk` packs no Java, so the dex is committed, embedded in the library and loaded at run time; after editing the Java, rebuild it with `scripts/build-android-dex.sh` (needs a JDK, e.g. Android Studio's `jbr`).

On Android the app starts in the project library (projects live in the app's storage, in folders); exports go to `Pictures/Rusty Painter` (images) or `Download/Rusty Painter` (PSD, SVG), time-lapses to Pictures as GIF (there's no ffmpeg on Android).

To build:

```bash
rustup target add aarch64-linux-android
cargo install cargo-apk
scripts/build-android.sh
```

If you have multiple Android SDK/NDK installs, ensure your environment points to the intended one (e.g. `ANDROID_HOME` and `ANDROID_NDK_HOME`).

**Release signing** is not committed. `cargo-apk` signs release builds with the keystore given by two environment variables:

```bash
export CARGO_APK_RELEASE_KEYSTORE=/path/to/release.keystore
export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=...
scripts/build-android.sh aarch64-linux-android release   # -> target/release/apk/rusty-painter.apk
```

(The commented-out `[package.metadata.android.signing.release]` block in `Cargo.toml` works too, but must never be committed.)

## iPad (iOS)
Experimental, and built without a Mac: the **iOS** workflow (`.github/workflows/ios.yml`) runs on GitHub's macOS runners, by hand only (*Actions* → *iOS* → *Run workflow*, on any branch). It lints the iOS build, runs the app in the iPad Simulator, and keeps a screenshot, the app's log and an unsigned `.ipa` as the run's `ios` artifact.

- `scripts/build-ios.sh [aarch64-apple-ios|aarch64-apple-ios-sim] [debug|release]` (on macOS) builds the bundle from the binary, `ios/Info.plist` and `ios/PrivacyInfo.xcprivacy`; there's no Xcode project.
- `src/ios.rs`: the Files and Photos pickers, the share sheet and exports (into the Documents folder: Files → On My iPad → Rusty Painter → Exports), through UIKit.
- `vendor-winit/src/platform_impl/ios/pen.rs`: the Apple Pencil's samples, every one UIKit coalesces, with their pressure, tilt, azimuth and roll.
- The app saves (settings, the autosave) as soon as it leaves the front: iOS ends background apps without warning.

**On your own iPad, for free:** install [SideStore](https://sidestore.io) once (from Linux, with `jitterbugpair` and AltServer-Linux), then open the workflow's `rusty-painter-ios-unsigned.ipa` in it: it signs the app with your own Apple ID. A free Apple ID's install lasts 7 days (SideStore refreshes it) and allows 3 sideloaded apps.

**TestFlight and the App Store** need a paid Apple Developer account: set `IOS_SIGN_IDENTITY` (an Apple Distribution identity in the keychain) and `IOS_PROFILE` (its App Store provisioning profile) for `build-ios.sh`, which then signs the `.ipa` for upload. The certificate, its CSR and the profile can all be made from Linux (`openssl`, then the developer website).

## Checks
The checks CI runs, to run before sending a change:
```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo check --locked --benches --features bench
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --document-private-items
```

## Benchmarks
```bash
cargo bench --features bench --bench app_bench               # every tool end to end on a painted 4000 px canvas
cargo bench --bench tools_bench                              # tool engines: mirror painting, shapes, selections, gradients, smart patch
cargo bench --bench brush_bench                              # brush stamping and compositing
cargo bench --bench tools_bench -- --save-baseline before    # save a run...
cargo bench --bench tools_bench -- --baseline before         # ...and compare a later one against it
```
Heavier single-shot timings live in ignored tests: `cargo test --release -- --ignored --nocapture`.

Wall-clock timings drift with the machine's state (a few percent, sometimes much more). To compare two builds exactly, count instructions on the fixed workloads in `examples/fixed_workloads.rs`, ideally in single-codegen-unit builds so inlining doesn't shift between them:
```bash
CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 cargo build --release --example fixed_workloads
perf stat -e instructions:u target/release/examples/fixed_workloads plain_stroke
```

## Profiling
```bash
scripts/flamegraph.sh            # use the app, close the window -> flamegraph.svg
FREQ=199 scripts/flamegraph.sh   # fewer samples
```
Needs `perf` and `cargo install flamegraph`. The script builds with frame pointers into `target/profiling` and records with `--call-graph fp --no-inline`. Plain `cargo flamegraph` uses DWARF call graphs and inline resolution, which here wrote a large `perf.data` and spent ~7 minutes at full CPU turning a 10 s recording into a graph.

## CI and releases
- **CI** (`.github/workflows/ci.yml`) runs on every push to `master` and on pull requests: `cargo fmt --check`, `clippy -D warnings`, tests, a bench build and a docs build on Linux and Windows, plus an Android compile check and `cargo audit`. The iPad build has its own workflow (see [iPad](#ipad-ios)).
- **Release** (`.github/workflows/release.yml`) runs the same checks first, then builds and publishes:
  - **By hand:** GitHub → *Actions* → *Release* → *Run workflow*. Enter the version and tick the platforms (Linux, Windows, Android). Untick *Publish* to only build; the files are then downloadable from the run page.
  - **By tag:** `git tag v0.2.0 && git push origin v0.2.0` builds all three and publishes release `v0.2.0`.
  - Assets: `rusty-painter-<version>-linux-x86_64.tar.gz`, `-windows-x86_64.zip`, `-android-arm64.apk`, `-linux-x86_64.flatpak`, plus a source zip.
- **Android signing in CI:** add two repository secrets (*Settings → Secrets and variables → Actions*):
  - `ANDROID_KEYSTORE_BASE64`: `base64 -w0 release.keystore`
  - `ANDROID_KEYSTORE_PASSWORD`: its password

  Without them the APK is signed with a throwaway key: it installs, but Android won't let a later release update it (different signature), so set them before sharing APKs. Keep the keystore safe: losing it means users must uninstall to upgrade.

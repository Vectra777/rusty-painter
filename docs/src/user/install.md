# Install

Releases are on the [GitHub releases page](https://github.com/Vectra777/rusty-painter/releases). Each one has:

| File | For |
|---|---|
| `rusty-painter-<version>-linux-x86_64.tar.gz` | Linux: the app, a menu entry and `install.sh` |
| `rusty-painter-<version>-linux-x86_64.flatpak` | Linux: a Flatpak bundle with ffmpeg inside (for video export) |
| `rusty-painter-<version>-windows-x86_64.zip` | Windows: the `.exe` (starts on a fresh Windows) and `ffmpeg.exe` |
| `rusty-painter-<version>-android-arm64.apk` | Android phones and tablets |
| `rusty-painter-<version>-source.zip` | The source code |

## Linux

**Tarball**: unpack it and run `./install.sh`. It puts the app in `~/.local/bin`, adds it to the applications menu with its icon, and can be run again to update. `./install.sh --uninstall` removes it.

**Flatpak**:

```bash
flatpak install --user rusty-painter-<version>-linux-x86_64.flatpak
flatpak run io.vectra.rustypainter
```

Video and time-lapse export need `ffmpeg`. The Flatpak carries its own; with the tarball, install it from your distribution.

## Windows

Unzip and run `rusty-painter.exe`. `ffmpeg.exe` is next to it for video export. If it doesn't start, see [Debugging on Windows](../dev/windows-debugging.md).

## Android

Install the APK (allow installs from your browser or file manager when Android asks). Android support is experimental. The app starts in the project library: projects live in the app's storage, in folders. Exports go to `Pictures/Rusty Painter` (images) or `Download/Rusty Painter` (PSD, SVG), and time-lapses to Pictures as GIF (there's no ffmpeg on Android).

## iPad

Experimental. There's no App Store build: the iOS workflow makes an unsigned `.ipa` that [SideStore](https://sidestore.io) signs with your own Apple ID. See [Building → iPad](../dev/building.md#ipad-ios).

## From source

With a Rust toolchain installed:

```bash
cargo run --release
```

See [Building](../dev/building.md) for the other platforms.

## Where your data is kept

Presets, settings, swatches, gradients, the autosave and any brush tips you add live in one folder per user, whichever folder the app is started from:

| System | Folder |
|---|---|
| Linux | `$XDG_DATA_HOME/rusty-painter`, else `~/.local/share/rusty-painter` |
| Windows | `%APPDATA%\rusty-painter` |
| macOS | `~/Library/Application Support/rusty-painter` |
| Android | the app's private storage |
| iPad | the app's Library/Application Support (exports and the log in its Documents, which the Files app shows) |

Set `RUSTY_PAINTER_DATA` to use another folder (a portable install, or a clean one to try things in). Delete the folder to start fresh.

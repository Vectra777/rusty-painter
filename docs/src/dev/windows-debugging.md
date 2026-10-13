# Windows debugging

## Finding out what happened

Release builds have no console window. The app writes its log to
`%APPDATA%\rusty-painter\rusty-painter.log` (the data folder; `RUSTY_PAINTER_DATA`
moves it). A log over 4 MB is moved to `rusty-painter.old.log` at start-up.

The log holds:
- the version, OS and architecture at start-up;
- the GPU adapter wgpu picked and the window's surface format;
- tablet start-up problems;
- errors that stop the app from starting, and panics with their backtrace.

`RUST_LOG` still sets the levels (the app's messages from `info` up and its libraries' warnings by default), e.g. `$env:RUST_LOG='debug'`.

## Narrowing a problem down

In PowerShell, before starting the app (remove them again afterwards):

| To rule out | Set |
| --- | --- |
| Tablet input (Windows Ink via octotablet) | `$env:RUSTY_PAINTER_DISABLE_TABLET='1'` |
| The GPU backend | `$env:WGPU_BACKEND='dx12'`, or `'vulkan'` if a Vulkan driver is installed |
| Your settings and presets | `$env:RUSTY_PAINTER_DATA='C:\some\empty\folder'` |

## When Windows won't run the program at all

If nothing is logged, the process may never have started. Windows Application
Control / Smart App Control can refuse to run an unsigned `rusty-painter.exe`
(error 4551; Code Integrity event 3077 in the Event Viewer under
Applications and Services Logs → Microsoft → Windows → CodeIntegrity →
Operational). The same policy can block Cargo's build scripts, so local builds
fail too. No change to the app can get around that: it needs a machine where
local builds may run, or a signed executable.

## Checking a build

With the MSVC Rust toolchain (Visual Studio's "Desktop development with C++"
and the Windows SDK) plus the `rustfmt` and `clippy` components:

```powershell
./scripts/check-windows.ps1
```

It runs the CI checks (format, clippy, tests, the bench build, a release build),
logging to `target/windows-debug/checks.log`, and stops at the first failure.
GPU tests skip when there's no adapter: read their output rather than taking a
pass as proof they ran.

Then try, at 100%, 125%, 150% and 200% display scaling:
1. The pen: pressure, the eraser end, hovering without painting, the stroke starting
   under the pen, lifting outside the canvas.
2. Alt+Tab in the middle of a mouse or pen stroke, and of a selection drag.
3. Space to pan and Alt to pick a colour while using the pen.
4. Opening, saving and exporting files whose paths have spaces and non-ASCII
   characters.

## History

These fixes came from the `windows` branch (commit `5f4a977`, made against
v0.0.4) and were ported onto the current code: the pen's position scaling on
scaled displays, the stroke start when the tablet reports the position before
the touch, releasing drags when the window loses focus, the start-up log, the
tablet off switch, and CI on Windows. Its other fixes were already in place
here (hovering doesn't paint, no mouse emulation, lifting outside the canvas,
Space/Alt with the pen). The brushes folder next to the executable was left
out: brushes live in the per-user data folder since 0.1.0.

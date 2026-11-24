# Contributing

1. Install Rust stable.
2. Download the model with `cargo run --release --bin download-model --manifest-path desktop/Cargo.toml`.
3. Before opening a change, run:

```powershell
cargo fmt --manifest-path desktop/Cargo.toml -- --check
cargo clippy --manifest-path desktop/Cargo.toml --all-targets --all-features -- -D warnings
cargo clippy --manifest-path packaging/tdt-setup/Cargo.toml --all-targets -- -D warnings
```

4. To build `dist/TDT-Setup.exe`: `powershell -ExecutionPolicy Bypass -File packaging/build-installer.ps1`.
5. To build `dist/TDT.app` and `dist/TDT-macOS.dmg` on a Mac:
   `packaging/macos/build-app.sh` (add `--universal` for Apple silicon and
   Intel; that needs the `aarch64-apple-darwin` and `x86_64-apple-darwin`
   Rust targets). CI runs the same clippy and tests on `macos-latest`.

### Platform code

Each module with system calls has a Windows file (`win.rs`) and a macOS file
(`mac.rs`) with the same functions, for example `hotkey/hook/win.rs` and
`hotkey/hook/mac.rs`. The macOS calls to AppKit and the C frameworks are in
`desktop/src/macos/`. Change both files together. The settings file keeps
Windows virtual-key codes on both systems (`keys.rs` maps the macOS key codes).

A Mac build with an ad-hoc signature is a new app for macOS after each
build, so macOS asks for the Microphone and Accessibility permissions again.
Remove the old TDT entry from Privacy & Security > Accessibility, then allow
the new one.

## Release

Set the version in `desktop/Cargo.toml` and push a matching tag, such as
`v0.7.0`. The `Release` workflow builds `TDT-Setup.exe` and attaches it with
`SHA256SUMS.txt`. It then builds `TDT-macOS.dmg` and attaches it too.

To sign and notarize the Mac app, add these repository secrets (a paid Apple
Developer account is necessary): `MACOS_CERTIFICATE_P12` (a base64
"Developer ID Application" certificate), `MACOS_CERTIFICATE_PASSWORD`,
`MACOS_SIGN_IDENTITY` (the identity name), and `MACOS_NOTARY_APPLE_ID`,
`MACOS_NOTARY_TEAM_ID`, `MACOS_NOTARY_PASSWORD` (an app-specific password).
Without them the app has an ad-hoc signature, and each user must click
**Open Anyway** once. TDT 0.2.32 and older need `SHA256SUMS.txt` to find the
update.

## Build with a local Trit

TDT uses a tagged version of [Trit](https://github.com/Hi9841/trit)
(`desktop/Cargo.toml`). To build TDT with a local Trit checkout:

```powershell
cargo build --release --manifest-path .\desktop\Cargo.toml --config "patch.'https://github.com/Hi9841/trit'.trit.path='..\trit'"
```

## Repository layout

| Folder | Contents |
|---|---|
| `desktop/` | The app for Windows and macOS (Rust and GPUI) and the speech benchmark. |
| `packaging/` | `TDT-Setup.exe` (install, update, uninstall) and its build script. `packaging/macos/` builds the Mac app and disk image. |
| `docs/` | The README images. `docs/images/capture.py` captures the app pictures again. |
| `site/` | The TDT website. |

## Pull requests

Keep model weights and generated build directories out of commits. Describe
user-visible behavior and include the checks you ran in pull requests.
By contributing you agree the change is licensed under the PolyForm
Noncommercial License 1.0.0 (see LICENSE).

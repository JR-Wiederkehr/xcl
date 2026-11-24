# Building this modified build

Everything that goes into `XCL-Setup.exe` is in this repository, except two Microsoft DLLs
(`dxcompiler.dll`, `dxil.dll`) that are taken from the Windows SDK at build time. There are no
prebuilt binaries, game files, keys or shader caches in the source.

## What you need

- Windows 10 (22H2) or Windows 11, x64
- [Rust](https://rustup.rs) stable, target `x86_64-pc-windows-msvc`
- Visual Studio 2022 Build Tools with "Desktop development with C++" (MSVC linker and a
  Windows 10/11 SDK – the SDK also provides `dxcompiler.dll` and `dxil.dll`)
- for the installer only: [Inno Setup 6](https://jrsoftware.org/isdl.php)

## Build

```powershell
git clone https://github.com/theaustinayers/xcl
cd xcl
git checkout v0.1.0-mod.12        # the tag of the release you want to check
powershell -ExecutionPolicy Bypass -File build.ps1
```

`build.ps1` runs `cargo build --release --locked -p xcl` (dependency versions come from
`Cargo.lock`), finds the newest installed Windows SDK that has the two DXC DLLs, and runs Inno
Setup on `installer/xcl.iss`. It prints the SHA-256 of both results:

- `target\release\xcl.exe`
- `dist\XCL-Setup.exe`

`build.ps1 -NoInstaller` builds only `xcl.exe` and does not need Inno Setup. You can run that
`xcl.exe` directly; put `dxcompiler.dll` and `dxil.dll` next to it.

## Checking a release binary

From build 6 on, the `XCL-Setup.exe` attached to a release is **not built on a developer's PC**.
It is built by GitHub Actions ([`.github/workflows/release.yml`](.github/workflows/release.yml))
from the release's tag on a GitHub-hosted Windows runner, and the build log is public under the
repository's *Actions* tab. Each build records a provenance attestation that ties the file to
the commit and the workflow run. To check a download:

```powershell
gh attestation verify XCL-Setup.exe -R theaustinayers/xcl
```

The release also carries `SHA256SUMS.txt`, and the same hashes are in the workflow run's
summary.

Releases up to build 5 were built locally; their source is at their tags, but nothing ties
those binaries to it other than building it yourself and comparing behaviour.

## Will my build have the same hash?

Not necessarily, and a different hash does not mean a different program. The Rust and MSVC
toolchains embed build paths and version details, the two DXC DLLs differ between Windows SDK
versions, and Inno Setup stores file times. To compare like with like, use the toolchain
versions printed in the workflow run's log – or rely on the attestation above, which does not
need a rebuild.

## Licence

XCL is GPL-2.0-only (`LICENSE`); third-party notices are in `THIRD_PARTY.md`. If you pass on a
binary you built, pass on the source it was built from as well.

# FileMan

A dual-pane file manager for Windows, built with Rust + [egui](https://github.com/emilk/egui).
Restart the app with folder settings & locations from where you left. Multiple user profiles supported. Configure buttons to open files with alternate applications.

## Requirements

- Windows 8.1+ (x64)
- [Rust toolchain](https://rustup.rs/) (stable)

## Build from source

```
git clone https://github.com/deepaksugandhi/FileMan.git
cd FileMan
cargo build --release
```

The binary is written to `target\release\fileman.exe`.

## Run

```
cargo run --release
```

## Test

```
cargo test
```

## Build the Windows installer

Requires [Inno Setup](https://jrsoftware.org/isinfo.php) (`ISCC.exe`).

```
cargo build --release
ISCC.exe installer.iss
```

The installer is written to `installer\FileMan-<version>-setup.exe`.

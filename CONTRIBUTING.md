# Contributing to Resource Monitor

Thank you for your interest in contributing! We welcome bug reports, feature requests, and pull requests.

## How to Contribute

1.  **Fork the repository**.
2.  **Create a feature branch**:
    ```bash
    git checkout -b feature/AmazingFeature
    ```
3.  **Commit your changes**:
    ```bash
    git commit -m 'Add some AmazingFeature'
    ```
4.  **Push to the branch**:
    ```bash
    git push origin feature/AmazingFeature
    ```
5.  **Open a Pull Request**.

## Reporting Bugs

Please open an [Issue](../../issues) and include:
- Windows Version.
- Steps to reproduce.
- Expected vs Actual behavior.
- Taskbar setup (position, auto-hide, Win11 alignment, scaling %).
- Your `%LOCALAPPDATA%\ResourceMonitor\settings.ini` if relevant.

## Development Setup

- Install Rust (stable) from https://rustup.rs with the MSVC toolchain.
- `cargo run` to start, `cargo test` for unit tests, `cargo build --release` for the final exe.
- Cross-compiling from Linux: `rustup target add x86_64-pc-windows-gnu`, install `mingw-w64`, then
  `cargo build --release --target x86_64-pc-windows-gnu` (linker set in `.cargo/config.toml`).
- Keep it light: no new dependencies without a strong reason; nothing heavy on the per-tick path.

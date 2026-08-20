# Contributing

Thanks for helping make `record` a better shell primitive.

## Development setup

You need Windows 10 or 11, the stable Rust toolchain, and a working default playback device.

```powershell
git clone https://github.com/irbto/record
cd record
rustup component add rustfmt clippy
cargo test --all-targets
cargo run -- doctor
```

For a short end-to-end capture:

```powershell
cargo run -- --no-tui --duration 1 --output smoke.mp3
```

Before opening a pull request, run:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

## Design constraints

- Typing bare `record` must remain the fastest and simplest path.
- The audio thread must never wait for the renderer.
- Windows builds must not require FFmpeg or another runtime encoder.
- Failure paths must either finalize a valid file or remove the incomplete output.
- New startup work needs a benchmark result, not only an intuition.

Please keep pull requests focused and include tests for behavior that can be exercised without audio hardware. Hardware-specific changes should also include the endpoint, format, and Windows version used for a smoke test.

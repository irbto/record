# Contributing

Thank you for your work on `record`.

## Development setup

Use Windows 10 or Windows 11. Install stable Rust. Make sure that the default playback device operates correctly.

```powershell
git clone https://github.com/irbto/record
cd record
rustup component add rustfmt clippy
cargo test --all-targets
cargo run -- doctor
```

Use this command for a short system test:

```powershell
cargo run -- --no-tui --duration 1 --output smoke.mp3
```

Before you open a pull request, run these commands:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
```

## Design constraints

- The bare `record` command must start audio capture as fast as possible.
- `AudioEvent::Started` is the primary startup boundary. Do not use the TUI mount time as a substitute.
- The TUI must show `RECORDING` only after WASAPI capture starts.
- The audio thread must never wait for the renderer.
- A Windows build must not use FFmpeg or a different runtime encoder.
- Each failure path must finalize a valid file or remove the incomplete file.
- Include benchmark results with each startup change.

## Documentation language

Use [ASD-STE100 Simplified Technical English, Issue 9](https://www.asd-ste100.org/assets/files/ASD-STE100_ISSUE9.pdf) for technical documentation.

Apply these rules:

- Use approved words or necessary technical terms.
- Use one term for one meaning.
- Use American English spelling.
- Use active voice.
- Write short and clear sentences.
- Use a maximum of 20 words in each instruction sentence.
- Use a maximum of 25 words in each descriptive sentence.
- Keep one topic in each paragraph.
- Do not use em dashes.

Keep each pull request focused. Add tests for behavior that does not need audio hardware.

For a hardware change, give the endpoint, audio format, and Windows version. Also give the result of a system test.

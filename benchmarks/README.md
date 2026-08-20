# Startup benchmarks

Startup is a product feature. `scripts/benchmark-startup.ps1` tracks two deliberately small metrics:

- **CLI process** is wall time for `record --version`, including Windows process creation and PowerShell measurement overhead.
- **WASAPI capture ready** is measured inside `record`, from the first instruction in `main` until the loopback client has started and the native MP3 sink is ready.

The optional `-Enforce` switch applies generous regression budgets of 50 ms and 100 ms respectively. Those budgets are local gates rather than hosted-CI gates because shared runners do not provide stable timing or a guaranteed playback endpoint.

## Reference result

Measured on 2026-08-20 with a release build, 3 warmups, and 30 runs:

| Environment | Metric | Median | Minimum |
|---|---|---:|---:|
| Windows 11 Pro N build 26200, Ryzen 5 5600X | CLI process | 11.22 ms | 10.32 ms |
| Windows 11 Pro N build 26200, Ryzen 5 5600X | WASAPI capture ready | 23.89 ms | 22.87 ms |

The stripped release executable was 2,221,568 bytes (2.12 MiB). Treat these numbers as a comparison point for this machine, not a universal guarantee.

Run the same gate locally:

```powershell
.\scripts\benchmark-startup.ps1 -Runs 30 -Enforce
```

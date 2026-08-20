# Startup benchmark

Audio capture ready is the primary startup metric. It measures the time until `record` is recording system audio.

The measurement starts at the first instruction in `main`. It stops after these actions are complete:

1. The MP3 writer opens.
2. The WASAPI loopback client starts.
3. The audio worker sends `AudioEvent::Started`.

The TUI changes from `STARTING` to `RECORDING` at this boundary.

The CLI process metric measures `record --version`. This metric includes Windows process creation and PowerShell measurement time.

## Reference result

The test used a release build, 3 warmup runs, and 30 measured runs. The test date was 2026-08-20.

| Environment | Metric | Median | Minimum | Budget |
|---|---|---:|---:|---:|
| Windows 11 Pro N build 26200, Ryzen 5 5600X | Audio capture ready | 23.89 ms | 22.87 ms | 100 ms |
| Windows 11 Pro N build 26200, Ryzen 5 5600X | CLI process | 11.22 ms | 10.32 ms | 50 ms |

The release executable was 2,221,568 bytes (2.12 MiB). These results apply to the reference computer. Results on other computers can be different.

## Run the benchmark

Use this command:

```powershell
.\scripts\benchmark-startup.ps1 -Runs 30 -Enforce
```

The `-Enforce` option applies the two budgets in the table. Run this test on a local computer with a playback endpoint.

Do not use a shared CI runner for startup limits. Runner load is not stable, and a playback endpoint is not always available.

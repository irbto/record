# Startup benchmark

Audio capture ready is the primary startup metric. It measures the time until a bare `record` command records system audio.

The measurement starts at the first instruction in `main`. It stops after these actions are complete:

1. The MP3 writer opens.
2. The session PCM cache opens.
3. The WASAPI loopback client starts.
4. The audio worker sends `AudioEvent::Started`.

The TUI changes from `STARTING` to `RECORDING` at this boundary.

The benchmark starts the capture probe without command-line arguments. It includes the default session setup and the fast argument path.

The CLI process metric measures `record --version`. This metric includes Windows process creation and PowerShell measurement time.

## Reference result

The test used a release build, 3 warmup runs, and 30 measured runs. The test date was 2026-08-20.

| Environment | Metric | Median | Minimum | Budget |
|---|---|---:|---:|---:|
| Windows 11 Pro N build 26200, Ryzen 5 5600X | Audio capture ready | 26.76 ms | 22.60 ms | 100 ms |
| Windows 11 Pro N build 26200, Ryzen 5 5600X | CLI process | 12.64 ms | 11.25 ms | 50 ms |

The release executable was 2,447,872 bytes (2.33 MiB). These results apply to the reference computer. Results on other computers can be different.

## Run the benchmark

Use this command:

```powershell
.\scripts\benchmark-startup.ps1 -Runs 30 -Enforce
```

The `-Enforce` option applies the two budgets in the table. Run this test on a local computer with a playback endpoint.

Do not use a shared CI runner for startup limits. Runner load is not stable, and a playback endpoint is not always available.

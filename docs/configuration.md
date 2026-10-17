# Configuration

8bf-host reads an optional TOML file from the platform's configuration folder: `~/.config/8bf-host/config.toml` on Linux, `~/Library/Application Support/8bf-host/config.toml` on macOS, and `%APPDATA%\8bf-host\config.toml` on Windows. `--config` or `EIGHTBF_HOST_CONFIG` name a different file. Settings are applied in this order, later ones winning: built-in defaults, the file, environment variables, command-line flags.

[config.example.toml](../config.example.toml) lists every setting with its default. The ones you are most likely to change are `plugin_dirs`, the `[workers]` and `[wine]` sections, and `[server]`. Unknown keys are reported as errors rather than ignored, so a typo does not silently fall back to a default. `8bf-host doctor` checks the result:

```console
$ 8bf-host doctor
ok   config file        /home/you/.config/8bf-host/config.toml
ok   host               eightbf-host 0.1.0 (natural FilterRecord layout), runs native filters in-process
ok   plug-in folder     /home/you/filters
ok   filters            3 found, 3 can run here
ok   wine               /usr/bin/wine
ok   win-x64 worker     /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe
note win-x86 worker     not found (/usr/local/bin/8bf-host-win-x86.exe); win-x86 filters cannot run
ok   server             listens on 127.0.0.1:8780, no token, filters isolated in workers
```

A `note` marks something optional: here, only 32-bit Windows filters are affected.

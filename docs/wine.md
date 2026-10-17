# Windows filters on Linux
## Windows filters on Linux

Linux has no native ecosystem of `.8bf` filters, so on Linux, 8bf-host runs Windows filters in the **Windows build of itself** under Wine. The Linux binary writes the job to a temporary folder, starts `wine 8bf-host.exe worker request.json`, and reads the result back. No Python or other runtime is involved. Setting this up takes three steps:

1. Install Wine from your distribution, for example `wine64`, or `wine` with WoW64 support.
2. Get the 64-bit Windows build, `8bf-host-<version>-win-x64.exe`, from a release, or build it yourself with `cargo bwin` (see [Development](development.md)).
3. Put it next to the Linux binary as `8bf-host-win-x64.exe`, or point the config file at it:

   ```toml
   [workers]
   win_x64 = "~/.local/lib/8bf-host/8bf-host-win-x64.exe"
   ```

`8bf-host doctor` then reports whether everything is in place.

8bf-host runs Wine in a prefix of its own, `~/.local/share/8bf-host/wine`, rather than Wine's default `~/.wine`, which may be 32-bit or set up for other programs. It creates the prefix on the first Windows filter run, which takes some extra seconds once. A `WINEPREFIX` in the environment is not used. To use another prefix, for example one where you installed runtimes a filter needs, set it in the config file:

```toml
[wine]
prefix = "~/.wine-filters"
```

A 32-bit prefix cannot run the 64-bit build. 8bf-host checks this before starting Wine: a filter that also has 32-bit code runs in the 32-bit build instead, and any other is refused with a message naming the prefix. On our test machine a warm run through Wine takes about 4.6 seconds, most of it Wine starting up; your timing will vary. For 32-bit filters, repeat the steps with the 32-bit build and `workers.win_x86`. Almost all current filters are 64-bit.

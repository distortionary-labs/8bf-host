# Windows filters on Linux
## Windows filters on Linux

Linux has no native ecosystem of `.8bf` filters, so on Linux, 8bf-host runs Windows filters in the **Windows build of itself** under Wine. The Linux binary writes the job to a temporary folder, starts `wine 8bf-host.exe worker request.json`, and reads the result back. No Python or other runtime is involved. Setting this up takes four steps:

1. Install Wine from your distribution, for example `wine64`, or `wine` with WoW64 support.
2. Get the 64-bit Windows build, `8bf-host-<version>-win-x64.exe`, from a release, or build it yourself with `cargo bwin` (see [Development](development.md)).
3. Put it next to the Linux binary as `8bf-host-win-x64.exe`, or point the config file at it:

   ```toml
   [workers]
   win_x64 = "~/.local/lib/8bf-host/8bf-host-win-x64.exe"
   ```

4. Initialize the Wine prefix once, so that Wine's first-run setup does not happen in the middle of a filter run:

   ```console
   $ WINEPREFIX=~/.wine-8bf wineboot --init
   ```

   and set the same prefix in the config file:

   ```toml
   [wine]
   prefix = "~/.wine-8bf"
   ```

`8bf-host doctor` then reports whether everything is in place. On our test machine a warm run through Wine takes about 4.6 seconds, most of it Wine starting up; your timing will vary. For 32-bit filters, repeat the steps with the 32-bit build and `workers.win_x86`. Almost all current filters are 64-bit.

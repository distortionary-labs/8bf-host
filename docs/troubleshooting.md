# Troubleshooting

**"... is a Windows filter. Install Wine and the Windows build of 8bf-host".** The filter needs the Wine setup described in [Windows filters on Linux](wine.md). `8bf-host doctor` shows which piece is missing.

**"... requires features this host does not provide".** The filter asked for a callback suite 8bf-host does not implement. Run it again with `--verbose`; the host log names every suite the filter requested and could not get, which helps when filing an issue.

**A Windows filter fails under Wine with no useful message.** Run the Windows build by hand to check that Wine starts it:

```console
$ WINEPREFIX=~/.wine-8bf wine ~/.local/lib/8bf-host/8bf-host-win-x64.exe --version
8bf-host 0.1.0
```

Then run the filter with `--verbose`, which includes the worker's own log. 8bf-host starts workers with `WINEDEBUG=-all` to keep Wine's own diagnostics out of that log; if the worker starts by hand but not from 8bf-host, compare the `wine` command and `WINEPREFIX` in the config file with the ones you used by hand.

**The output looks banded after a 16-bit run.** The filter probably only supports 8-bit images and ran on an 8-bit copy. `8bf-host show` lists the modes it supports. `--no-8bit-fallback` turns the fallback into an error, so batch jobs fail loudly instead.

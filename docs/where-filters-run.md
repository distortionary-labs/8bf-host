# Where filters run

8bf-host picks one of three places for each filter. `--run-mode` (or `run_mode` in the config file) can override the choice.

**In this process.** This is the default whenever the filter has code for the running binary: a 64-bit Windows filter in the 64-bit Windows build, an Apple silicon (or universal) filter on an Apple silicon Mac. It is the fastest option and the only one in which a filter's dialog can appear in front of the calling program. A crashing filter takes the process with it.

**In a worker process of the same build.** With `--run-mode worker`, and in the server by default, the same binary starts a copy of itself to run the job. Communication goes through a temporary folder holding the request, the pixels and the response; progress is reported on the worker's standard output, and cancelling creates a file the worker checks for.

**In a worker of a different build.** A filter whose architecture does not match runs in a matching build of 8bf-host:

- Windows filters on Linux run in the 64- or 32-bit Windows build under Wine, as described in [Windows filters on Linux](wine.md).
- 32-bit filters on 64-bit Windows run in the 32-bit Windows build (`workers.win_x86`).
- Intel-only filters on Apple silicon run under Rosetta, in an Intel or universal build of 8bf-host (`workers.mac_x64`, or this binary if it is universal).

When no worker is configured, 8bf-host looks for one next to its own executable, named `8bf-host-win-x64.exe`, `8bf-host-win-x86.exe` or `8bf-host-mac-x64`.

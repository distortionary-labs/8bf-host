# 8bf-host

8bf-host runs Photoshop-compatible filter plug-ins (`.8bf` files) without Photoshop. It has two front ends: a command line that filters image files, and an HTTP server that filters images sent to it. It runs on Linux, Windows and macOS. On Linux, Windows filters run under Wine.

## Why this exists

Over the past thirty years many image filters were sold as Photoshop plug-ins: sharpening and noise reduction tools, film emulation, G'MIC's 8bf build, and plenty of small utilities whose vendors no longer exist. Photoshop defined a C interface for these plug-ins in the early 1990s, and other editors adopted it so they could load the same binaries. Paint Shop Pro, Paint.NET (through PSFilterPdn) and GIMP (through pspi) all did.

All of those hosts live inside an image editor, though. If you want to apply a filter to a folder of scans, run one as a step in a build, or offer it to other programs as a service, you need a host that is not attached to an editor. 8bf-host is that host.

### How a filter host works

A filter plug-in is a shared library. It exports one entry point, and the host calls it repeatedly with a large C structure called the `FilterRecord`. That structure describes the image and the rectangles the filter wants to read and write. It also carries about two dozen tables of callbacks, which the filter uses to allocate memory, query properties, report progress and draw previews. A host has to build that structure byte for byte and answer every callback correctly; most of the code in this project does that.

A filter has to run in a process whose operating system and processor match its binary. That rule shapes most of the design below.

## What works

- Filters on 8- and 16-bit RGB and grayscale images, with or without alpha.
- Selections. You pass a mask image; the filter receives it, and the result is blended through it as Photoshop's "auto mask" does, including soft edges.
- Presets. A filter's settings can be saved after a run (with or without showing its dialog) and replayed later without the dialog, in batch or over HTTP.
- 16-bit images with filters that only declare 8-bit support. The host runs the filter on an 8-bit copy and keeps the full 16-bit data for every sample the filter left unchanged. You can turn this off.
- The callback suites filters use in practice: handles, buffers, resources, properties, color services, the descriptor (scripting) suite, and the PICA basic suite with the handle, buffer, UI-hooks and property suites. Previews through `displayPixels` work on Windows (GDI) and macOS (Core Graphics).
- Image files in PNG, JPEG, TIFF, WebP, BMP and PNM. PNG, TIFF and PNM keep 16 bits per channel.

### Known limitations

These are worth knowing about before you depend on 8bf-host:

- **Color modes.** Only RGB and grayscale are supported. CMYK and Lab images, and floating-point (HDR) files, need converting first.
- **Not implemented:** channel ports, image services (resampling callbacks), floating selections, absolute channel indexing, and layer masks. A filter that requires one of these reports an error, typically "requires features this host does not provide".
- **Metadata.** ICC profiles and EXIF data are not copied from the input to the output.
- **In-process crashes.** A filter running in-process shares the process with 8bf-host. If it crashes, the process goes with it. The server isolates every run in a worker process by default for this reason; the command line does not, since one crashed command only loses one run. Use `--run-mode worker` if you want isolation there too.
- **Dialogs need a desktop.** `--dialog` shows the filter's own window, so it needs a display. The server never shows dialogs.
- **Plug-in compatibility.** Some commercial filters check for Photoshop specifically, require activation through Adobe components, or rely on undocumented behavior. Expect some of them to refuse to run.
- **macOS is tested in CI only.** The test suite runs on a Mac in CI, but the macOS-specific parts (bundle loading, previews, Rosetta workers) have not been tried with real Adobe filters.

### Check your plug-in licenses

Being able to load a filter does not mean its license lets you use it this way. Read the EULA of every 8bf plug-in before running it with 8bf-host. Some commercial licenses only permit use inside a supported graphics program, forbid batch or other non-interactive use, or restrict server and network deployment. Running a filter from the command line, in a build or behind the HTTP server can fall outside those terms. You are responsible for making sure your use complies with each plug-in's license.

## Installation

You need Rust 1.88 or later to build from source:

```console
$ cargo install --path .
$ 8bf-host --version
8bf-host 0.1.0
```

Release builds are static Linux binaries for x86-64 and ARM, 64-bit and 32-bit Windows builds, and a universal macOS binary; see [Releasing](#releasing). The Windows builds need only system DLLs. The macOS binary is not signed or notarized, so macOS blocks it on first run until you allow it (or run `xattr -d com.apple.quarantine` on it).

### Windows filters on Linux

Linux has no native ecosystem of `.8bf` filters, so on Linux, 8bf-host runs Windows filters in the **Windows build of itself** under Wine. The Linux binary writes the job to a temporary folder, starts `wine 8bf-host.exe worker request.json`, and reads the result back. No Python or other runtime is involved. Setting this up takes four steps:

1. Install Wine from your distribution, for example `wine64`, or `wine` with WoW64 support.
2. Get the 64-bit Windows build, `8bf-host-<version>-win-x64.exe`, from a release, or build it yourself with `cargo bwin` (see [Development](#development)).
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

## Using the command line

The command line has six commands: `list`, `show`, `run`, `about`, `serve` and `doctor`. Each one has `--help`.

### Finding filters

8bf-host looks for filters in folders, including their subfolders. On Windows and macOS the usual Adobe plug-in folders are added automatically when they exist, as is `~/8bf-plugins` everywhere. You can add folders three ways: `--plugin-dir` on the command line, the `EIGHTBF_HOST_PLUGIN_PATH` environment variable (separated like `PATH`), or `plugin_dirs` in the config file.

```console
$ 8bf-host --plugin-dir ~/filters list
443b91e33c0e  Wine/Error   under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
8c8224326f39  Wine/Invert  under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
3776e356a9f9  Wine/Suites  under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
```

Each line has three parts. The first column is the filter's **id**, a short hash of its file and name that stays the same between runs. Next is the filter's category and name. The last column says where the filter would run, or why it cannot run here. Scan results are cached, so later runs only re-read plug-in files whose size or modification time changed; `--rescan` ignores the cache.

Commands that take a filter accept any of four forms: the id, `Category/Name`, the name alone (if it is unique), or the path to a plug-in file. Names are matched without regard to case. If a name is ambiguous, the error lists the candidates with their ids.

`show` describes one filter:

```console
$ 8bf-host show Wine/Invert
Invert
  id:         8c8224326f39
  category:   Wine
  file:       /home/you/filters/testfilter.8bf
  code:       win-x64
  modes:      rgb8, gray8
  about box:  yes
  runs:       under Wine (win-x64, /home/you/.local/lib/8bf-host/8bf-host-win-x64.exe)
```

`modes` lists the image types the filter declares support for. This one only supports 8-bit images, so a 16-bit input would run on an 8-bit copy, as described above.

### Running a filter

The usage is:

```text
8bf-host run [OPTIONS] <FILTER> <INPUT> <OUTPUT>
```

The simplest form runs the filter with its default settings, without its dialog:

```console
$ 8bf-host run Wine/Invert photo.png inverted.png
```

The output format comes from the output file's extension, or `--format`. The output keeps the input's layout: an RGBA PNG stays RGBA, and 16 bits stay 16 bits where the format allows.

Either file can be `-` for standard input or output, which lets 8bf-host sit in a pipeline. With standard output there is no extension to read, so name the format:

```console
$ curl -s https://example.com/photo.png | 8bf-host run 8c8224326f39 - - --format png > out.png
```

To filter part of an image, pass a rectangle as `X,Y,WIDTH,HEIGHT`, or a mask image the same size as the input, where white is selected and grey is partly selected:

```console
$ 8bf-host run Wine/Invert photo.png out.png --rect 100,50,400,300
$ 8bf-host run Wine/Invert photo.png out.png --mask selection.png
```

Some filters use the foreground and background colors or the image resolution; `--foreground`, `--background` (as `#rrggbb`) and `--resolution` set them.

### Settings and presets

Most filters have a dialog, and the settings you choose there are what makes a filter useful. 8bf-host handles this in two steps. First, run the filter once with `--dialog` and save its settings to a preset:

```console
$ 8bf-host run "Acme/Unsharp Mask" sample.png check.png --dialog --save-preset unsharp.json
```

Then replay the preset without the dialog, as many times as you like:

```console
$ for f in scans/*.tif; do
>   8bf-host run "Acme/Unsharp Mask" "$f" "sharp/$(basename "$f")" --preset unsharp.json
> done
```

A preset holds two things: the filter's own parameter block, which is opaque binary data, and its **descriptor**, the named settings that scripting-aware filters record for Photoshop actions. The descriptor is readable JSON, so you can adjust a setting without opening the dialog again:

```json
{
  "format": "8bf-host-preset",
  "version": 1,
  "filter": { "name": "Unsharp Mask", "category": "Acme", "id": "1a2b3c4d5e6f" },
  "parameters": "NBIAAAEAAAA=",
  "descriptor": [
    { "key": "Amnt", "type": "long", "value": 7 }
  ]
}
```

Whether editing works depends on the filter. Filters that use descriptors read them when replaying, so a changed `value` takes effect. Filters that do not use them only read the opaque `parameters`.

### Repeated runs

`--repeat N` runs the filter N times in one process, each run on the last one's output, the way Photoshop's Last Filter command (Ctrl+F) reapplies a filter: the dialog shows at most once, and later runs reuse the settings the filter kept. The output is the last run's image.

Photoshop also unloads a filter's code after every run and loads it again for the next. `--unload-between-runs` does the same, which catches filters that leave code running after they return, such as a thread pool. On Windows the host then reserves the freed address range with no access, so such code faults instead of running whatever DLL lands there next, and it looks for threads that started inside the unloaded filter. Finding any fails the run, because in Photoshop they crash the application when they wake:

```console
$ 8bf-host run --run-mode in-process Acme/Halftone photo.png out.png --repeat 2 --unload-between-runs
[host] halftone.8bf unloaded with 16 of its threads still running in its freed code at 0x6ffffdff0000 (+0x22f000)
error: Halftone... left 16 threads running after it was unloaded; hosts that unload filters, as Photoshop does, crash when they wake
```

A filter that pins itself in memory reports that it is still mapped after unloading, and passes. Unloading needs the filter to run in this process, so pass `--run-mode in-process`; a worker process unloads everything when it exits anyway. `--pause MS` waits after each run, for code that misbehaves only some time after the filter returns.

### Exit codes

Scripts can tell failures apart by the exit code:

| Code | Meaning |
|------|---------|
| 0    | Success |
| 1    | The filter ran and reported an error |
| 2    | Invalid arguments |
| 3    | No filter matches the name, or several do |
| 4    | The filter cannot run on this system with the current configuration |
| 5    | An image could not be read or written |
| 6    | The configuration file is invalid |
| 130  | Cancelled (Ctrl+C, or the filter's own Cancel button) |

The first Ctrl+C asks the filter to stop, which well-behaved filters do at their next progress check. A second Ctrl+C exits at once. `--json` prints a summary of the run (status, where it ran, the host log) for scripts that want more than an exit code.

## Using the HTTP server

`8bf-host serve` offers the same filters over HTTP:

```console
$ 8bf-host serve
8bf-host 0.1.0 serving 3 filters on http://127.0.0.1:8780 (isolated workers)
```

The API is small. The full reference, with every option and error, is in [docs/http-api.md](docs/http-api.md).

```console
$ curl -s localhost:8780/v1/health
{"status":"ok","version":"0.1.0"}
$ curl -s localhost:8780/v1/filters
{"filters":[{"archs":["win-x64"],"category":"Wine","format":"pe","has_about_box":true,"id":"443b91e33c0e", ...snip...
$ curl -s --data-binary @photo.png -H "Content-Type: image/png" \
    localhost:8780/v1/filters/8c8224326f39/run -o inverted.png
```

The last request sends the image as the request body and receives the filtered image back. Options go in the query string (`?format=jpeg&rect=0,0,100,100`). To send a mask or a preset along with the image, use a `multipart/form-data` request instead:

```console
$ curl -s -F image=@photo.png -F mask=@selection.png -F preset=@unsharp.json \
    localhost:8780/v1/filters/1a2b3c4d5e6f/run -o out.png
```

Errors come back as JSON with an HTTP status that reflects the cause, for example `404` for an unknown filter and `422` when the filter itself fails:

```console
$ curl -s --data-binary @photo.png localhost:8780/v1/filters/443b91e33c0e/run
{"error":{"kind":"filter_failed","message":"Could not complete the filter because the test filter failed on purpose"}}
```

### Running the server safely

A filter is native code, and the server runs it on request. A few defaults follow from that:

- **Isolation.** Every run happens in a separate worker process, so a filter that crashes fails one request instead of stopping the server. `--no-isolate` runs filters inside the server process instead. That saves the cost of starting a process per run, and it is reasonable for filters you have tested. In-process runs go through a single thread, one at a time, because few filters are written to run concurrently.
- **Loopback by default.** The server listens on `127.0.0.1:8780`. It refuses to listen on any other address unless a token is set with `EIGHTBF_HOST_TOKEN` (or `server.token`); clients then send `Authorization: Bearer <token>`. If the network is trusted, `--insecure-no-auth` overrides the check. The token is not a substitute for TLS: put a reverse proxy such as Caddy in front of the server if it is reachable beyond one machine.
- **No uploads of code.** Clients pick filters by id or name from the scanned folders. A request cannot name a file for the server to load.
- **Limits.** Request bodies are limited to 256 MB, images to 100 megapixels, and runs to 300 seconds; a run that takes longer is cancelled and the client receives `504`. All three are configurable.

## Configuration

8bf-host reads an optional TOML file from the platform's configuration folder: `~/.config/8bf-host/config.toml` on Linux, `~/Library/Application Support/8bf-host/config.toml` on macOS, and `%APPDATA%\8bf-host\config.toml` on Windows. `--config` or `EIGHTBF_HOST_CONFIG` name a different file. Settings are applied in this order, later ones winning: built-in defaults, the file, environment variables, command-line flags.

[config.example.toml](config.example.toml) lists every setting with its default. The ones you are most likely to change are `plugin_dirs`, the `[workers]` and `[wine]` sections, and `[server]`. Unknown keys are reported as errors rather than ignored, so a typo does not silently fall back to a default. `8bf-host doctor` checks the result:

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

## Where filters run

8bf-host picks one of three places for each filter. `--run-mode` (or `run_mode` in the config file) can override the choice.

**In this process.** This is the default whenever the filter has code for the running binary: a 64-bit Windows filter in the 64-bit Windows build, an Apple silicon (or universal) filter on an Apple silicon Mac. It is the fastest option and the only one in which a filter's dialog can appear in front of the calling program. A crashing filter takes the process with it.

**In a worker process of the same build.** With `--run-mode worker`, and in the server by default, the same binary starts a copy of itself to run the job. Communication goes through a temporary folder holding the request, the pixels and the response; progress is reported on the worker's standard output, and cancelling creates a file the worker checks for.

**In a worker of a different build.** A filter whose architecture does not match runs in a matching build of 8bf-host:

- Windows filters on Linux run in the 64- or 32-bit Windows build under Wine, as described in [Installation](#windows-filters-on-linux).
- 32-bit filters on 64-bit Windows run in the 32-bit Windows build (`workers.win_x86`).
- Intel-only filters on Apple silicon run under Rosetta, in an Intel or universal build of 8bf-host (`workers.mac_x64`, or this binary if it is universal).

When no worker is configured, 8bf-host looks for one next to its own executable, named `8bf-host-win-x64.exe`, `8bf-host-win-x86.exe` or `8bf-host-mac-x64`.

## Troubleshooting

**"... is a Windows filter. Install Wine and the Windows build of 8bf-host".** The filter needs the Wine setup described in [Installation](#windows-filters-on-linux). `8bf-host doctor` shows which piece is missing.

**"... requires features this host does not provide".** The filter asked for a callback suite 8bf-host does not implement. Run it again with `--verbose`; the host log names every suite the filter requested and could not get, which helps when filing an issue.

**A Windows filter fails under Wine with no useful message.** Run the Windows build by hand to check that Wine starts it:

```console
$ WINEPREFIX=~/.wine-8bf wine ~/.local/lib/8bf-host/8bf-host-win-x64.exe --version
8bf-host 0.1.0
```

Then run the filter with `--verbose`, which includes the worker's own log. 8bf-host starts workers with `WINEDEBUG=-all` to keep Wine's own diagnostics out of that log; if the worker starts by hand but not from 8bf-host, compare the `wine` command and `WINEPREFIX` in the config file with the ones you used by hand.

**The output looks banded after a 16-bit run.** The filter probably only supports 8-bit images and ran on an 8-bit copy. `8bf-host show` lists the modes it supports. `--no-8bit-fallback` turns the fallback into an error, so batch jobs fail loudly instead.

## Project layout

```text
src/
  main.rs, cli.rs        Entry point and argument definitions
  commands/              One file per subcommand
  config.rs              Configuration file and environment
  catalog.rs             Filter scanning, cache and lookup
  runner.rs              Choosing in-process or a worker; driving workers
  protocol.rs            The job folder format shared with workers
  imageio.rs             Image files to filter input and back
  preset.rs              Preset files
  server.rs              The HTTP API
  host/                  The filter host; no files, config or network
    structs.rs           #[repr(C)] mirrors of FilterRecord and the suites
    session.rs           The selector loop and every host callback
    context.rs           Per-thread state the C callbacks reach
    memory.rs            Handles (with real master pointers), buffers, blocks
    descriptors.rs       The scripting descriptor suite
    image.rs, pixels.rs  Rectangle and plane copies, padding, sampling
    masking.rs           Auto-masking through the selection
    pipl.rs, pe.rs,      PiPL parsing from Windows DLLs, Mac resource
    rsrc.rs,             forks and native sidecar files
    discovery.rs
tests/
  fixtures/testfilter.c  A filter written for the tests
  session.rs             The host against the test filter
  cli.rs, server.rs      The binary and the HTTP API, end to end
  wine.rs                Windows filters under Wine (opt-in)
fuzz/
  fuzz_targets/          One cargo-fuzz target per parser
  seeds/                 Valid inputs each target starts from
```

The `host` module has no knowledge of files, configuration or the network. It takes an image in memory, runs a filter and returns the result. Everything else is built on top of it.

### A note on structure layout

The Photoshop SDK declares `FilterRecord` inside `#pragma pack(push, 4)` on Windows, for both 32- and 64-bit builds, and with natural alignment on macOS. On 64-bit the two layouts differ: 608 bytes against 648, and most pointer fields sit at different offsets. 8bf-host uses `#[repr(C, packed(4))]` when built for Windows and `#[repr(C)]` elsewhere. The `pack4` cargo feature selects the Windows layout on any platform, so the test suite can check both layouts on Linux against the C compiler.

## Development

The development environment is described in `devenv.nix`; the Rust version and targets come from `rust-toolchain.toml`, which CI reads too. Run `direnv allow` once to enter it automatically, or prefix commands with `devenv shell --`.

```console
$ cargo test
$ cargo test --features pack4
```

The tests compile `tests/fixtures/testfilter.c` with the system C compiler (`$CC`, `cc`, `gcc` or `clang`; on Windows, MinGW's `gcc`). Its ten entry points each exercise one part of the host: tiling, plane-at-a-time access, `advanceState`, edge padding, sampling, masks, the PICA suites, descriptors, progress, cancellation and error strings. The second command repeats the suite with the Windows `FilterRecord` layout. On Windows, which always uses that layout, run `cargo test --target x86_64-pc-windows-gnu` once; CI does this on a Windows runner.

The Wine tests are opt-in, because they need Wine, zig and the Windows build:

```console
$ cargo bwin
$ EIGHTBF_HOST_TEST_WINE_WORKER=$PWD/target/x86_64-pc-windows-gnu/release/8bf-host.exe \
    cargo test --test wine
```

They compile the test filter into a real Windows DLL with PiPL resources, so the PE reader is tested against a real binary, and run it through Wine and the Windows build exactly as Linux users do. `EIGHTBF_HOST_TEST_WINE` and `EIGHTBF_HOST_TEST_WINEPREFIX` select the Wine binary and prefix.

### Fuzzing

The parsers that read untrusted bytes have [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets in `fuzz/`: `pe`, `rsrc` and `pipl` for filter binaries and resources, `descriptor` for descriptor handles, `preset` and `config` for the JSON and TOML files, and `image` for image and mask uploads. They need a nightly toolchain:

```console
$ rustup toolchain install nightly
$ cargo install --locked cargo-fuzz
$ bin/fuzz            # every target for 60 seconds each
$ bin/fuzz pe 600     # one target for ten minutes
```

Each run starts from the seed inputs in `fuzz/seeds/<target>` and keeps what it finds in `fuzz/corpus/<target>`, which is not committed. Crashing inputs land in `fuzz/artifacts/<target>`; `cargo +nightly fuzz run <target> <file>` replays one.

The cargo aliases in `.cargo/config.toml` build the release targets: `cargo br` and `cargo barm` for the static x86-64 and ARM Linux binaries, `cargo bwin` and `cargo bwin32` for the Windows builds. All four use `cargo-zigbuild`.

### Releasing

`bin/release` checks formatting, lints and tests, builds both static Linux binaries and both Windows builds into `dist/` with checksums, and tags the version. It requires a clean working tree and a `CHANGELOG.md` entry for the version. It stops before publishing and prints the commands for that, so a release can be inspected before it goes out.

The universal macOS binary is cross-compiled as well, with `cargo zigbuild --target universal2-apple-darwin` inside cargo-zigbuild's Docker image, which carries a macOS SDK; `bin/release` needs Docker for that step. The arm64 half is ad-hoc signed by the linker, so it starts on Apple silicon, but neither half is signed with a Developer ID or notarized. It needs macOS 13 or later. That SDK comes from a third-party repackaging, and Apple's SDK license only permits use on Apple hardware; build on a Mac instead if that matters for your distribution.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](./LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

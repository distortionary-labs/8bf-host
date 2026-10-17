# Changelog

## Unreleased

## 0.1.0 - 2026-10-17

Initial release.

- `8bf-host run` filters image files (PNG, JPEG, TIFF, WebP, BMP, PNM, and
  standard input and output), with selections from a mask image or a
  rectangle, presets saved from and replayed to the filter, and distinct
  exit codes per failure class.
- `8bf-host list`, `show`, `about` and `doctor` find filters, describe them
  and check what the system can run.
- `8bf-host serve` offers the filters over HTTP, with each run isolated in a
  worker process by default, loopback-only listening unless a token is set,
  and limits on body size, image size and run time.
- Filters run in-process when their architecture matches, otherwise in a
  matching build of 8bf-host: the Windows build under Wine on Linux, the
  32-bit Windows build for 32-bit filters, and Rosetta for Intel-only
  filters on Apple silicon.
- Filter discovery from Windows DLLs, macOS bundles and native libraries
  with `.pipl` sidecars, with a scan cache.
- 8- and 16-bit RGB and grayscale images with or without alpha, auto-masking,
  filter-case handling, padding and input sampling, and an 8-bit fallback
  for 8-bit-only filters.
- Handle, buffer, resource, property, color, descriptor and PICA suites;
  previews through `displayPixels` on Windows and macOS.
- Fuzz targets for the PE, resource fork, PiPL, descriptor, preset,
  configuration and image parsers, with seed inputs, under `fuzz/`. Run
  them with `bin/fuzz`.
- Images with a zero width or height, which PAM files can declare, are
  rejected when they are read instead of when the filter run starts.
- `8bf-host run --repeat N` runs a filter N times, each on the last run's
  output with its settings and no dialog, as Photoshop's Last Filter does.
- `--unload-between-runs` unloads the filter after each run, as Photoshop
  does. On Windows the freed range is reserved no-access, and threads the
  filter left running in it fail the run. `--pause MS` waits after each run.
- Release builds add a static ARM Linux binary (`linux-arm64`) and a
  universal macOS binary (macOS 13 or later), both cross-compiled with
  cargo-zigbuild.
- The test suite runs on Windows, with the test filter built as a DLL, and
  CI runs it on a Windows runner.
- Hardening against malformed input and misbehaving filters:
  - Filter requests are checked before a run: 1 or 3 color channels, a plane
    count that matches, a mask the image's size and a positive resolution.
    `--resolution` now rejects zero, negative and non-numeric values, as the
    server already did, and colors such as `#+f+f+f` are rejected.
  - Allocations a filter asks for (handles, buffers, blocks and pixel
    buffers) are capped at the 1 GB of free space the host reports, so an
    absurd request returns an out-of-memory error instead of getting the
    process killed.
  - Growing a locked handle keeps the old block alive until it is unlocked,
    so a filter's stale pointer no longer writes into freed memory.
  - Descriptors nest at most 16 levels deep.
  - Mac resource maps can't expand into copies of the same data, and plug-in
    metadata, resource files and PE files are read with size limits; links
    to devices are refused.
  - A worker's stderr is kept up to its last 1 MB, and over-long progress
    lines are skipped.
  - Catalog cache entries that name a file other than the one they are
    cached under are ignored and the file is scanned again.
  - `inColumnBytes` and `outColumnBytes` match the planes a filter asked
    for. They always counted every plane, so a filter that read or wrote
    one plane of a 4-plane image by stepping with them overran the host's
    buffer.
  - Asking for the same tile again hands the filter its buffer pointers
    again, and a change of padding alone fills the tile anew.
  - A handle disposed twice is refused instead of being treated as a
    Windows `HGLOBAL`, which could free host memory.
  - Running out of memory while making a handle for a descriptor, alias or
    property returns an out-of-memory error instead of a null handle.
  - A preset whose parameters can't be loaded fails the run instead of
    running with the last settings.
  - `colorServices` refuses an info block smaller than its `infoSize`
    requires, and `displayPixels` reads only the fields an older pixel map
    has.
  - Descriptors in presets, requests and worker results nest at most 16
    levels deep.
  - PE resources that overlap so that reading them would copy more than
    the file holds are refused, and each resource directory is read once.
  - FIFOs and devices in plug-in folders are skipped without being opened,
    so they can't hang a scan.
  - A plug-in whose resources or PiPLs all fail to read is reported to the
    scan's error output instead of vanishing.
  - Files a worker leaves behind are read with size limits and must be
    regular files; the output image must be exactly the input's size.
  - Once a worker exits, the host waits at most 2 seconds for its output
    pipes, which a process it started may hold open.
- `run` replaces an existing output file atomically, keeping its
  permissions, so a failed write no longer destroys the original (which may
  be the input image).
- Server: the `jobs` limit now covers decoding the upload as well as the
  run, and a slot stays taken until the filter finishes even if the client
  disconnects. Masks are checked for size before decoding. An in-process
  filter that ignores cancellation gets a `504` 10 seconds after the
  timeout, and a panic in the host fails that run only. Rescans run one at
  a time.
- Server: in-process runs wait in their own queue, bounded by `jobs`,
  instead of holding a run slot, so a filter that never returns can't
  starve worker runs; a full queue gets a `503`. Queued runs that are past
  their deadline, or whose client left, are dropped. Waiting for a slot
  gives up with a `503` after `server.timeout_secs`.
- Configuration: `server.max_upload_mb`, `server.max_pixels` and
  `server.timeout_secs` must be at least 1, the timeout at most one day,
  and a blank `server.token` counts as no token.

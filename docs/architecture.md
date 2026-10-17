# Project layout

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

## A note on structure layout

The Photoshop SDK declares `FilterRecord` inside `#pragma pack(push, 4)` on Windows, for both 32- and 64-bit builds, and with natural alignment on macOS. On 64-bit the two layouts differ: 608 bytes against 648, and most pointer fields sit at different offsets. 8bf-host uses `#[repr(C, packed(4))]` when built for Windows and `#[repr(C)]` elsewhere. The `pack4` cargo feature selects the Windows layout on any platform, so the test suite can check both layouts on Linux against the C compiler.

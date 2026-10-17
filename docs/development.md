# Development

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

## Fuzzing

The parsers that read untrusted bytes have [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets in `fuzz/`: `pe`, `rsrc` and `pipl` for filter binaries and resources, `descriptor` for descriptor handles, `preset` and `config` for the JSON and TOML files, and `image` for image and mask uploads. They need a nightly toolchain:

```console
$ rustup toolchain install nightly
$ cargo install --locked cargo-fuzz
$ bin/fuzz            # every target for 60 seconds each
$ bin/fuzz pe 600     # one target for ten minutes
```

Each run starts from the seed inputs in `fuzz/seeds/<target>` and keeps what it finds in `fuzz/corpus/<target>`, which is not committed. Crashing inputs land in `fuzz/artifacts/<target>`; `cargo +nightly fuzz run <target> <file>` replays one.

The cargo aliases in `.cargo/config.toml` build the release targets: `cargo br` and `cargo barm` for the static x86-64 and ARM Linux binaries, `cargo bwin` and `cargo bwin32` for the Windows builds. All four use `cargo-zigbuild`.

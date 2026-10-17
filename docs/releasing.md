# Releasing
## Releasing

Add changes to the `## Unreleased` section of `CHANGELOG.md` as you go. To release, run:

```console
$ bin/release 0.2.0
```

The script sets the version in `Cargo.toml`, renames `## Unreleased` to `## 0.2.0 - <today>`, runs formatting, lints, tests and `cargo publish --dry-run`, commits, tags `v0.2.0` and pushes. Without an argument it releases the version already in `Cargo.toml`. It refuses to run with uncommitted changes, off `main`, or out of step with `origin/main`, and undoes its edits if a check fails.

Pushing the tag is the release. CI runs the full test suite, cross-compiles the static Linux binaries and both Windows builds with cargo-zigbuild, builds the universal macOS binary on a macOS runner, and creates the GitHub release with the binaries, a `SHA256SUMS` file and the version's changelog section as notes. If the repository has a `CARGO_REGISTRY_TOKEN` secret, it also publishes to crates.io.

The macOS binary needs macOS 13 or later. Its arm64 half is ad-hoc signed by the linker, so it starts on Apple silicon, but neither half is signed with a Developer ID or notarized.

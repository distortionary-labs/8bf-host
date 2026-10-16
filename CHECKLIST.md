# Rust Crate Checklist

## Project Setup

- [x] `mtem create rust <name>` ran — `devenv.nix`, `devenv.yaml` present
- [x] `.envrc` created (run `direnv allow` once in your own terminal)
- [ ] `devenv.lock` generated (created on the first `devenv shell`)
- [x] `Cargo.toml` has `edition`, `rust-version`, `license`, `description`, `repository`, `documentation`

## Code Standards

- [ ] All public items have `///` doc comments (most do; some FFI struct fields and constants do not)
- [ ] Every public function has at least one `# Examples` doctest (only the pure helpers do)
- [x] `# Errors` section present on `Result`-returning public functions
- [x] Custom error type in `src/error.rs` using `thiserror`
- [x] No `unwrap()` / `expect()` in library code
- [x] Every `unsafe` block has a `// SAFETY:` comment

## Testing

- [x] Unit tests in `#[cfg(test)]` blocks alongside the code
- [x] Integration tests in `tests/` (session, CLI, HTTP, Wine)
- [x] Doc tests pass (`cargo test --doc`)
- [ ] Coverage >90% (not measured yet)

## Tooling

- [x] `cargo fmt --check` exits 0
- [x] `cargo clippy --all-targets -- -D warnings` exits 0 (Linux, Windows and macOS targets, both layouts)
- [x] `cargo test` and `cargo test --features pack4` exit 0
- [x] `cargo doc --no-deps` exits 0 with no warnings
- [x] `cargo audit` exits 0

## Release

- [x] `CHANGELOG.md` updated under a dated heading
- [x] Version set in `Cargo.toml` (semver)
- [x] `cargo publish --dry-run` succeeds (also run by `bin/release`)
- [ ] Git tag created and pushed (run by `bin/release`)
- [ ] GitHub release created (CI, on the tag)
- [ ] `cargo publish` ran (CI, when `CARGO_REGISTRY_TOKEN` is set)

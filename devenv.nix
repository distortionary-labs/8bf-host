{ pkgs, lib, config, inputs, ... }:

{
  # gcc builds the C test filter; zig and cargo-zigbuild cross-compile the
  # Windows workers and the static Linux binary.
  packages = with pkgs; [ git libyaml openssl gcc zig cargo-zigbuild ];

  # Toolchain version, components and targets come from ./rust-toolchain.toml
  # so the dev shell and CI resolve identically.
  languages.rust = {
    enable = true;
    toolchainFile = ./rust-toolchain.toml;
  };

  # Rust's windows-gnu targets call MinGW's dlltool; zig ships one.
  scripts."x86_64-w64-mingw32-dlltool".exec = ''exec zig dlltool "$@"'';
  scripts."i686-w64-mingw32-dlltool".exec = ''exec zig dlltool "$@"'';

  enterShell = ''
  '';

  git-hooks.hooks = {
    rustfmt.enable = true;
  };
}

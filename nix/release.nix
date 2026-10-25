# `nix run .#release -- <version>`: packaging/release.sh with its tools. gh
# and rbw come from the user's system, with their credentials and agent.
{
  writeShellApplication,
  cargo,
  coreutils,
  curl,
  gawk,
  git,
  gnugrep,
  gnused,
  jq,
  rustc,
}:
writeShellApplication {
  name = "release";
  runtimeInputs = [cargo coreutils curl gawk git gnugrep gnused jq];
  # The workflows' RUST_VERSION must match it.
  runtimeEnv.FLAKE_RUSTC = rustc.version;
  text = builtins.readFile ../packaging/release.sh;
}

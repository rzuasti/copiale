# `nix run .#release -- <version>`: packaging/release.sh with its tools. rbw
# comes from the user's system, to match its running agent.
{
  writeShellApplication,
  cargo,
  coreutils,
  curl,
  gawk,
  gh,
  git,
  gnugrep,
  gnused,
  jq,
  rustc,
}:
writeShellApplication {
  name = "release";
  runtimeInputs = [cargo coreutils curl gawk gh git gnugrep gnused jq];
  # The workflows' RUST_VERSION must match it.
  runtimeEnv.FLAKE_RUSTC = rustc.version;
  text = builtins.readFile ../packaging/release.sh;
}

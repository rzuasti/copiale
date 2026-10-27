# `nix run .#windows-vm -- <command>`: packaging/windows/vm.sh with its
# tools; see docs/windows-vm.md. gh comes from the user's system, with their
# credentials.
{
  writeShellApplication,
  cdrtools,
  coreutils,
  curl,
  gnugrep,
  gnused,
  libxml2,
  quickemu,
  virt-viewer,
}:
writeShellApplication {
  name = "windows-vm";
  runtimeInputs = [cdrtools coreutils curl gnugrep gnused libxml2 quickemu virt-viewer];
  text = builtins.readFile ../packaging/windows/vm.sh;
}

#!/usr/bin/env bash
# Windows 11 test VM (#97): the Enterprise evaluation in quickemu, to try
# windows.yml's builds from Linux. See docs/windows-vm.md.
#
#   windows-vm create [iso]     unattended install; downloads the ISO unless given
#   windows-vm start            SPICE console
#   windows-vm fetch [run-id]   a windows.yml build (latest by default) into the share
#   windows-vm delete           removes the VM, keeping the ISO

GITHUB_REPO=rzuasti/copiale
EVAL_PAGE=https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise
EVAL_HOST=https://software-static.download.prss.microsoft.com/
STATE=${XDG_STATE_HOME:-$HOME/.local/state}/copiale/windows-vm
# Its name decides how quickget treats it: Windows, unattended.
ISO_NAME=windows-11-enterprise-eval.iso
ISO=$STATE/$ISO_NAME
VM=$STATE/windows-11
CONF=$VM.conf
# \\10.0.2.4\qemu in the guest.
SHARE=$STATE/share

say() { printf '\033[1m==> %s\033[0m\n' "$*" >&2; }
die() {
  printf 'windows-vm: %s\n' "$*" >&2
  exit 1
}

usage() {
  die "usage: windows-vm create [iso] | start | fetch [run-id] | delete"
}

running() {
  local pid
  pid=$(head -n 1 "$VM/windows-11.pid" 2>/dev/null) && kill -0 "$pid" 2>/dev/null
}

# The en-US x64 ISO from the evaluation center, into $ISO.
download_iso() {
  local link target=""
  say "Finding the Windows 11 Enterprise evaluation ISO"
  for link in $(curl -fsSL --proto =https --max-filesize 1M "$EVAL_PAGE" |
    sed 's/&amp;/\&/g' |
    grep -o 'https://go\.microsoft\.com/fwlink/?[^"]*culture=en-us&country=us' |
    sort -u); do
    target=$(curl -fsSIL --proto =https -o /dev/null -w '%{url_effective}' "$link") || continue
    [[ $target == "$EVAL_HOST"*CLIENTENTERPRISEEVAL_OEMRET_x64FRE_en-us.iso ]] && break
    target=""
  done
  [ -n "$target" ] || die "can't find the ISO; download it from $EVAL_PAGE and run: windows-vm create <iso>"
  say "Downloading ${target##*/}"
  curl -fL --proto =https -C - -o "$ISO.part" "$target"
  mv "$ISO.part" "$ISO"
}

# quickget's answer file, for evaluation media: no product key (Pro's
# doesn't fit it), activation, and guest access to the SMB share.
patch_unattended() {
  local xml=$VM/unattended/autounattend.xml
  sed -i -e '/<ProductKey>.*<\/ProductKey>/d' -e '/<ProductKey>/,/<\/ProductKey>/d' "$xml"
  # shellcheck disable=SC2016 # PowerShell's $true and $false
  sed -i 's|^\( *\)</FirstLogonCommands>|\1  <SynchronousCommand wcm:action="add">\
\1    <CommandLine>cscript //B %WINDIR%\\System32\\slmgr.vbs /ato</CommandLine>\
\1    <Description>Activate the evaluation</Description>\
\1    <Order>90</Order>\
\1  </SynchronousCommand>\
\1  <SynchronousCommand wcm:action="add">\
\1    <CommandLine>powershell -NoProfile -Command "Set-SmbClientConfiguration -EnableInsecureGuestLogons $true -RequireSecuritySignature $false -Force"</CommandLine>\
\1    <Description>Guest access to the host share</Description>\
\1    <Order>91</Order>\
\1  </SynchronousCommand>\
&|' "$xml"
  ! grep -q ProductKey "$xml" || die "$xml: product key left"
  grep -q EnableInsecureGuestLogons "$xml" || die "$xml: no FirstLogonCommands to add to"
  xmllint --noout "$xml"
  mkisofs -quiet -J -o "$VM/unattended.iso" "$VM/unattended/"
}

create() {
  [ $# -le 1 ] || usage
  [ ! -e "$VM" ] && [ ! -e "$CONF" ] || die "a VM exists; run windows-vm delete first"
  mkdir -p "$STATE" "$SHARE"
  if [ -n "${1:-}" ]; then
    [ -f "$1" ] || die "no such file: $1"
    # quickget moves it into the VM: link or copy, keeping the original.
    rm -f "$ISO"
    ln "$1" "$ISO" 2>/dev/null || cp --reflink=auto "$1" "$ISO"
  elif [ ! -f "$ISO" ]; then
    download_iso
  fi

  say "Creating the VM"
  (cd "$STATE" && quickget --create-config "$VM" "$ISO" --unattended) || {
    [ -f "$VM/$ISO_NAME" ] && mv "$VM/$ISO_NAME" "$ISO"
    die "quickget failed; run windows-vm delete, then create again"
  }
  patch_unattended
  cat >>"$CONF" <<EOF
disk_size="64G"
ram="8G"
tpm="on"
secureboot="off"
display="spice"
viewer="remote-viewer"
public_dir="$SHARE"
EOF

  say "Installing; it restarts a few times and signs in as Quickemu (password quickemu)"
  quickemu --vm "$CONF"
}

start() {
  [ $# -eq 0 ] || usage
  [ -f "$CONF" ] || die "no VM; run windows-vm create"
  # Stopped since create: installed. Without the ISO it boots from disk,
  # and quickemu stops pressing a key to start setup.
  running || sed -i '/^iso=/d' "$CONF"
  quickemu --vm "$CONF"
}

# gh comes from the user's system, signed in.
fetch() {
  local run
  [ $# -le 1 ] || usage
  [ -d "$SHARE" ] || die "no VM; run windows-vm create"
  run=${1:-$(gh run list -R "$GITHUB_REPO" --workflow windows.yml --status completed --limit 1 \
    --json databaseId --jq '.[0].databaseId // empty')}
  [ -n "$run" ] || die "no windows.yml run; start one: gh workflow run windows.yml -R $GITHUB_REPO --ref main"
  tmp=$(mktemp -d "$STATE/fetch.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  say "Downloading run $run"
  gh run download "$run" -R "$GITHUB_REPO" --name copiale.exe --dir "$tmp"
  mv -f "$tmp/copiale.exe" "$SHARE/copiale.exe"
  gh run view "$run" -R "$GITHUB_REPO" --json headBranch,headSha \
    --jq '"Built from \(.headBranch) \(.headSha[:10])"' >"$SHARE/copiale.txt"
  say "$(cat "$SHARE/copiale.txt"): \\\\10.0.2.4\\qemu\\copiale.exe"
}

delete() {
  [ $# -eq 0 ] || usage
  ! running || die "the VM is running; shut Windows down first"
  [ -f "$VM/$ISO_NAME" ] && mv "$VM/$ISO_NAME" "$ISO"
  rm -rf "$VM" "$CONF"
  say "Deleted; the ISO stays for the next create"
}

command=${1:-}
[ $# -eq 0 ] || shift
case $command in
  create) create "$@" ;;
  start) start "$@" ;;
  fetch) fetch "$@" ;;
  delete) delete "$@" ;;
  *) usage ;;
esac

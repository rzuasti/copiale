# Windows test VM

Windows builds come from GitHub Actions on the mirror
(github.com/rzuasti/copiale, `windows.yml`); a Windows 11 VM on a Linux
workstation runs them. No Windows dev environment, and no dev tools in the
guest.

```sh
nix run .#windows-vm -- create      # once
gh workflow run windows.yml -R rzuasti/copiale --ref main
nix run .#windows-vm -- fetch       # after the run
nix run .#windows-vm -- start
```

State lives in `$XDG_STATE_HOME/copiale/windows-vm/` (default
`~/.local/state/copiale/windows-vm/`).

## Needs

- x86_64 Linux with KVM.
- About 80 GB of disk (ISO, virtio drivers, a 64 GB disk image) and 8 GB of
  RAM for the guest.
- `gh`, logged in (`gh auth login`), for `fetch`.

## Create

`create` makes a Windows 11 Enterprise evaluation VM in quickemu and starts
its unattended install. The evaluation is activated (90 days), so dark/light
mode and the accent colour can be changed, unlike an unactivated Windows.

- **ISO**: downloaded (en-US, ~7 GB) from the
  [evaluation center](https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise).
  If that fails, download it there by hand and pass it:
  `create <iso>` (the file is linked or copied, not moved).
- **Install**: quickget's answer file, without its product key (the
  evaluation brings its own), with activation and guest access to the share
  added. Windows restarts a few times and signs in as `Quickemu` (password
  `quickemu`). The screen can stay black for several minutes in setup's
  first stage; the disk image grows meanwhile. Shut it down when it reaches
  the desktop; the next `start` boots from the disk. An install stopped
  before the desktop can't resume: `delete`, then `create`.
- **Drivers**: quickget's virtio download can get a bot check page instead
  of the ISO, leaving setup with no disks; `create` downloads it again then.

## Test a CI build

1. Run `windows.yml` (`gh workflow run windows.yml -R rzuasti/copiale --ref
   <branch>`). Its `build` job uploads `copiale.exe` (statically linked C
   runtime; kept 30 days); `check` runs fmt, clippy and the tests.
2. `fetch [run-id]`: the given run's build, or the latest finished run's,
   into the share, with the branch and commit in `copiale.txt`.
3. `start`: the SPICE console (`remote-viewer`). In the guest, copy
   `copiale.exe` from `\\10.0.2.4\qemu` to the desktop and run it there:
   QEMU's Samba lists the share but refuses Windows' DFS paths to its files.
4. Logs: in `%LOCALAPPDATA%\korba\Copiale\data\copiale.log` (the previous
   run's in `copiale.log.old`); follow with
   `Get-Content -Wait $env:LOCALAPPDATA\korba\Copiale\data\copiale.log`.
   Set the filter from PowerShell, e.g.
   `$env:COPIALE_LOG = "copiale_core=debug"; .\copiale.exe`.

## Known limits

- No GPU in the guest (software rendering): frame timing means nothing, and
  transparency effects may be off.
- x86_64 hosts only.
- The guest's resolution follows the window; for larger text, set
  Settings → System → Display → Scale in Windows.
- When the evaluation runs out: `slmgr /rearm` in an administrator prompt
  (then restart), or recreate the VM: `delete` (keeps the ISO), then
  `create`.

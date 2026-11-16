<p align="center">
  <img src="assets/logo.svg" alt="Stagehand — a gentoo installer" width="320">
</p>

<h1 align="center">Stagehand</h1>
<p align="center"><strong>Gentoo, one step at a time.</strong></p>

A keyboard-first, guided **Gentoo Linux installer written in Rust**. Inspired by
[archinstall](https://github.com/archlinux/archinstall) and [YaST](https://yast.opensuse.org/),
Stagehand lets you configure an installation in a clean terminal interface,
review every decision, and install only after explicit confirmation.

> **Early development (v0.5.0).** The installer has not yet passed an end-to-end
> UEFI VM boot test. **Do not run it against a disk containing valuable data.**
> Use a disposable virtual machine until the installation and boot path are validated.

## One screen, all the important choices

```text
┌ Welcome ────────────────────────────────────────────────────────────────┐
│ STAGEHAND              Gentoo installer · v0.5.0 · AMD64 / x86_64       │
├ Installation settings ───┬ Details ─────────────────────────────────────┤
│ › Storage                │ Welcome to Stagehand.                       │
│     /dev/vda · Btrfs     │                                            │
│ › System                 │ Installation profile:                       │
│     Gentoo · Base Stage3 │ • Systemd Stage3: base / desktop            │
│ › Kernel & boot          │ • Kernel: gentoo-kernel-bin                  │
│     bin · systemd-boot   │ • Bootloader: systemd-boot                  │
│ › Network                │ • Filesystem: Btrfs / ext4                  │
│     systemd-networkd    │ • Network: networkd / NetworkManager        │
│ › Users                  │ • Portage: binary packages preferred        │
│     dan                 │                                            │
│ › Portage                │ Enter any category to configure it.         │
│     Binpkg · Auto jobs  │ The final ERASE confirmation is required.   │
│ › System tools           │                                            │
│     nano · Cron off     │                                            │
│ ▶ Install Gentoo         │                                            │
├ Keys ────────────────────┴────────────────────────────────────────────┤
│ ↑↓/j/k Navigate   Enter Open   q Quit                                  │
└────────────────────────────────────────────────────────────────────────┘
```

The main menu is organized into **Storage**, **System**, **Kernel & boot**, **Network**, **Users**, **Portage** and **System tools** submenus, plus a separate final installation action.

The interface uses **ratatui + crossterm** with a compact Gruvbox-inspired color
scheme. It runs entirely in the terminal and does not require Python, Ansible,
a graphical environment, or a desktop environment.

### What you configure

- Target disk with a non-destructive inspection of partition tables and filesystem signatures. Existing data is clearly marked; mounted, read-only and removable disks are unavailable.
- Hostname, timezone, console keymap, UTF-8 locale and standard or desktop systemd Stage3.
- Kernel package and bootloader in the **Kernel & boot** submenu.
- Filesystem: Btrfs (default) or ext4; optional 4 GiB swap partition (minimum 16 GiB target).
- Portage: prefer binary packages (default) or compile from source; automatic, 2-job or 4-job MAKEOPTS.
- Networking: systemd-networkd with wired DHCP (default) or NetworkManager with Ethernet/Wi-Fi support.
- System tools: optional cronie daemon and a preferred text editor (none, nano or vim).
- Local administrator account and root credentials, with password confirmation.
- Optional SSH with public-key authentication (password SSH and root SSH disabled).
- Final installation review and a mandatory typed `ERASE /dev/...` confirmation.

### What Stagehand installs

The initial installation profile follows the base system from
[dme86/gentoo-ansible](https://github.com/dme86/gentoo-ansible):

| Component | Default |
| --- | --- |
| Architecture | Native amd64 or arm64 |
| Init | systemd; base Stage3 (default) or desktop Stage3 |
| Partitioning | GPT, 512 MiB FAT32 ESP, optional 4 GiB swap, remaining space for root |
| Subvolumes | `@` mounted at `/`, `@home` at `/home` |
| Filesystem | Btrfs with zstd compression (default) or ext4 |
| Kernel | `gentoo-kernel-bin` (default) or `gentoo-kernel` (source build) |
| Bootloader | systemd-boot (default) or GRUB, UEFI |
| Networking | systemd-networkd with wired DHCP (default) or NetworkManager |
| Package management | Portage, binary packages preferred; optional source builds; configurable MAKEOPTS |
| Console and locale | Configurable keymap; en_US.UTF-8 (default), de_DE.UTF-8 or C.UTF-8 |

Stagehand detects its **native live-system architecture** at runtime (`x86_64` → `amd64`, `aarch64` → `arm64`) and downloads only the selected base or desktop systemd stage3. No Gentoo ISO download is necessary: boot a Linux live system first. Cross-architecture installations are intentionally unsupported.

The official systemd stage3 is downloaded and **SHA-256 verified before any disk
writes occur**. Installation logs are written to `/tmp/stagehand-install.log`,
with credential input excluded from the log. Failures trigger a best-effort
unmount of the installation tree.

Anvil, rst, greetd and other personal desktop tooling from
`gentoo-ansible/post_install.yml` are **not** installed in this first version.
The base installer is intentionally independent of that configuration.

## Build and try it

Build on a Linux machine of the **same architecture** as the intended Gentoo
installation environment:

```sh
git clone https://github.com/dme86/Stagehand.git
cd Stagehand
cargo build --release
sudo ./target/release/stagehand
```

The Gentoo minimal ISO does not necessarily include the Rust toolchain. Build
the binary on a separate Linux host and copy it into the **Gentoo live
environment**, or prepare a custom live image. A macOS-native binary is not
supported, even on an ARM Mac.

The live environment needs UEFI boot, a working network connection, and:
`lsblk`, `swapon`, `parted`, `udevadm`, `mkfs.vfat`, `mkfs.btrfs`,
`btrfs`, `mount`, `umount`, `chroot`, `tar`, `curl`, `wipefs`, and `sync`.
`mkfs.ext4` and `mkswap` are additionally required when those options are selected.
The chosen disk must have at least 8 GiB; a larger disk is strongly
recommended for Portage. Stagehand refuses mounted, read-only and removable target disks, and checks active swap.
Its disk inspection distinguishes **known signatures / partitions**, **no known signatures**, and **unknown**. No-signature detection does not guarantee an empty disk; full-disk erasure always requires explicit confirmation.

**First try Stagehand inside a disposable UEFI QEMU/UTM virtual machine**, with
a dedicated, empty virtual disk. Do not boot your everyday Linux system and
point the installer at an attached physical drive.

## Architecture and installer media

Stagehand does **not** download or create a live ISO. It is a Linux binary designed to run *inside an already booted live environment* such as the official Gentoo minimal ISO. Architecture detection selects the correct **Stage3 archive**, not a boot image. An optional custom Stagehand bootable image may be provided in a future release.


## Installation settings

The left-hand main menu deliberately stays small:

- **Storage** — installation disk, Btrfs/ext4 filesystem and optional 4 GiB swap.
- **System** — hostname, timezone, keymap, UTF-8 locale, base/desktop Stage3.
- **Kernel & boot** — `gentoo-kernel-bin` or `gentoo-kernel`; systemd-boot or GRUB (UEFI).
- **Network** — systemd-networkd or NetworkManager, optional key-only SSH and public key.
- **Users** — administrator name/password and root password.
- **Portage** — binary packages preferred or source builds; automatic, two or four parallel build jobs.
- **System tools** — nano/vim/no additional editor, optional cronie.
- **Install Gentoo** — full summary and typed `ERASE /dev/...` confirmation.

Selecting `gentoo-kernel` builds the distribution kernel from source and can be slower, while `gentoo-kernel-bin` is recommended for a first installation. Stagehand configures `sys-kernel/installkernel` USE flags to match the chosen bootloader and verifies boot artifacts before reporting success.

## Keyboard controls

| Key | Action |
| --- | --- |
| `↑` / `↓`, `j` / `k` | Navigate settings |
| `Enter` | Select, edit, or save |
| `Esc` | Return to main menu |
| `r` | Refresh disk list |
| `Esc` | Return to parent submenu |
| `q` | Quit from main menu |
| `L` | Switch between phase progress and live logs during/after installation |
| `q` / `Esc` (installing) | Detach the TUI **without stopping the installer** |
| `Enter` (finished/failed) | Return to installer setup |

Once you confirm the full-disk erase, Stagehand starts a **detached installer worker**.
The TUI only displays its progress, and pressing `L` toggles between completed
installation phases and the live log. Closing Stagehand with `q` or `Esc`
**does not stop the worker**. Run Stagehand again in the *same live environment*
to reattach to the current worker, or inspect its final success/failure result.

The progress gauge counts **completed installation phases out of 12**. A source
kernel compilation may take a long time, so Stagehand does not invent a
time-based percentage. Failed commands and cleanup errors remain available
in the log even after the TUI exits. The worker exits after installation
success or failure; it is not a permanent daemon.

## Detached installer worker

The installer is split into two parts inside a **single Rust executable**:

- **TUI (ratatui):** collects installation choices, checks confirmation, shows
  completed phases, and reads worker status and logs. It does not partition or
  build packages itself.
- **Worker process:** launched after the typed ERASE confirmation, in a separate
  Linux session. It survives closing the TUI, performs installation commands,
  and writes a redacted status snapshot after each phase change.

The configuration, including credentials, is passed **only over a bounded
anonymous stdin pipe** into the worker. It is never placed into command-line
arguments, environment variables, status JSON, or installation logs. The TUI
clears its credential strings after successfully launching the worker.

Worker runtime files:

| File | Purpose |
| --- | --- |
| /run/stagehand/status.json | Atomic status snapshot: PID, state, phase, number completed, message or error |
| /run/stagehand/active.lock | Exclusive worker lock against concurrent installation |
| /tmp/stagehand-install.log | Full command output, credential stdin redacted |

The worker status directory is accessible only to root. Runtime status is
**ephemeral** and persists only for the current live boot. A forcibly killed
worker may leave the lock in place: investigate mounts, mounted chroot
filesystems and the installation log before manually clearing the lock. A new
installation never automatically resumes a partially written filesystem.

### Start, detach, reattach

1. Boot the Gentoo live environment and run Stagehand as root.
2. Complete the installer choices and confirm the selected disk with ERASE.
3. Watch the phase bar; press L to inspect live command output.
4. Optionally press q or Esc to leave the TUI; the worker continues.
5. Run Stagehand again from the same live system to reattach.

Note: a successful Rust build and unit tests do **not** validate the complete
Gentoo installation/boot process. Only use disposable VM disks until the full
UEFI install and reboot workflow is verified.

## Current scope and next milestones

This is a **first implementation**, not a fully validated general-purpose
Gentoo installer. In particular:

- UEFI only; no BIOS/legacy boot.
- Whole-disk installs only; no partition preservation, LUKS or dual-boot.
- Wired DHCP only; Wi-Fi and advanced network setup are not yet configured.
- systemd/UEFI only; Stage3 flavor, Btrfs/ext4, kernel, swap, network manager and UEFI bootloader are selectable.
- No automated integration test booting the resulting disk yet.
- Re-running the installation is destructive; no resume support.
- SHA-256 validation protects downloaded archives against accidental corruption,
  but signed Gentoo OpenPGP verification is not yet implemented.
- Kernel and bootloader combinations need to be verified on real UEFI VMs.

Roadmap: automated QEMU boot testing, signed artifact verification, additional
init profiles, advanced locale and network configuration, package
selection, disk encryption and optional Anvil + rst desktop integration.

## Comparison with gentoo-ansible

The base installer follows the systemd/UEFI profile in
[gentoo-ansible/install.yml](https://github.com/dme86/gentoo-ansible/blob/main/install.yml).
Stagehand additionally configures `/etc/hosts`, a fresh first-boot `/etc/machine-id`,
`locale.gen`, `locale.conf` and the selected UTF-8 locale using `locale-gen`.
The Gentoo Stage3's existing Portage profile and binhost settings are retained.

The Ansible installer is dedicated to a QEMU guest and enables key-only **root SSH**
for remote VM administration. Stagehand intentionally requires explicit disk
selection and uses key-only SSH for the named administrator; root SSH is disabled.

The separate `post_install.yml` installs a personalized Anvil/rst/greetd graphical
desktop and is not a requirement for a functional Gentoo base installation.

OpenRC, custom Portage profiles/USE flags, advanced partitioning, LUKS,
Secure Boot, Wi-Fi, static IP configuration and opt-in desktop installation remain
outside the supported profile. Rust CI is not a substitute for a real boot
test: use a disposable UEFI QEMU disk before trusting a new configuration.


## Gentoo Handbook coverage

Stagehand v0.5.0 incorporates additional choices from the official
[Gentoo AMD64 Handbook](https://wiki.gentoo.org/wiki/Handbook:AMD64).
Both AMD64 and ARM64 offer standard and desktop systemd Stage3 archives; desktop
Stage3 includes extra development tools, **not a preinstalled graphical environment**.
Stagehand downloads and verifies the selected archive before modifying the disk.

For network management, choose the default `systemd-networkd` with wired DHCP,
or `NetworkManager` for Ethernet/Wi-Fi management. NetworkManager installs with
Wi-Fi support, but **does not connect to a wireless SSID during installation**:
configure wireless credentials after the first boot. An active wired network is
required by the installer itself.

Portage can keep automatic `MAKEOPTS` (the recommended setting) or request
`-j2` / `-j4`. The Handbook suggests allowing approximately 2 GiB RAM per
simultaneous build job. The separate System tools submenu offers the optional
`cronie` service and nano/vim editors. Systemd already supplies journald and
timer units, so no separate logging daemon or scheduler is required by default.

For now the installer remains a guided **systemd/UEFI, whole-disk** installer.
The Handbook also covers OpenRC, multilib/no-multilib Stage3, custom Gentoo
profiles/USE flags, Legacy BIOS boot, detailed partitioning, Secure Boot and
manual kernel compilation. Those need dedicated installation workflows and
integration testing rather than exposing settings that Stagehand cannot reliably
apply yet. **Rust CI validates code, not successful Gentoo boot.**

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The installer uses separate modules for configuration validation, block-device
discovery, installation execution and terminal UI. Privileged operations happen
only after confirmation; command execution passes arguments directly to
processes rather than interpolating settings into shell scripts.

## Branding

The official Stagehand logo is included as a scalable SVG with a transparent
background in [`assets/logo.svg`](assets/logo.svg). The violet Gentoo-inspired
emblem, download symbol and wordmark follow the approved Stagehand artwork.

## License

MIT.

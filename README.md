<p align="center">
  <img src="assets/logo.svg" alt="Stagehand — a gentoo installer" width="320">
</p>

<h1 align="center">Stagehand</h1>
<p align="center"><strong>Gentoo, one step at a time.</strong></p>

A keyboard-first, guided **Gentoo Linux installer written in Rust**. Inspired by
[archinstall](https://github.com/archlinux/archinstall) and [YaST](https://yast.opensuse.org/),
Stagehand lets you configure an installation in a clean terminal interface,
review every decision, and install only after explicit confirmation.

> **Early development (v0.3.0).** The installer has not yet passed an end-to-end
> UEFI VM boot test. **Do not run it against a disk containing valuable data.**
> Use a disposable virtual machine until the installation and boot path are validated.

## One screen, all the important choices

```text
┌ Welcome ───────────────────────────────────────────────────────────────┐
│ STAGEHAND              Gentoo installer · v0.2.0 · AMD64 / x86_64      │
├ Installation settings ───┬ Details ────────────────────────────────────┤
│ › Storage                │ Welcome to Stagehand.                      │
│     /dev/vda             │                                            │
│ › System                 │ Installation profile:                      │
│     gentoo, Europe/...   │ • Official Gentoo systemd Stage3           │
│ › Kernel & boot          │ • gentoo-kernel-bin (default)               │
│     bin · systemd-boot   │ • systemd-boot (default)                   │
│ › Network                │ • GPT / UEFI / Btrfs                       │
│     Wired DHCP          │ • AMD64 or ARM64 detected at runtime       │
│ › Users                  │                                            │
│     dan                 │ Enter a category to configure its options.│
│ ▶ Install Gentoo         │ Nothing written before final confirmation.│
├ Keys ────────────────────┴────────────────────────────────────────────┤
│ ↑↓/j/k Navigate   Enter Open   q Quit                                 │
└───────────────────────────────────────────────────────────────────────┘
```

The main menu is organized into **Storage**, **System**, **Kernel & boot**, **Network**, **Users** and **Portage** submenus, plus a separate final installation action.

The interface uses **ratatui + crossterm** with a compact Gruvbox-inspired color
scheme. It runs entirely in the terminal and does not require Python, Ansible,
a graphical environment, or a desktop environment.

### What you configure

- Target disk with a non-destructive inspection of partition tables and filesystem signatures. Existing data is clearly marked; mounted, read-only and removable disks are unavailable.
- Hostname, timezone, console keymap and a UTF-8 system locale.
- Kernel package and bootloader in the **Kernel & boot** submenu.
- Filesystem: Btrfs (default) or ext4; optional 4 GiB swap partition (minimum 16 GiB target).
- Portage: prefer binary packages (default) or compile from source.
- Local administrator account and root credentials, with password confirmation.
- Optional SSH with public-key authentication (password SSH and root SSH disabled).
- Final installation review and a mandatory typed `ERASE /dev/...` confirmation.

### What Stagehand installs

The initial installation profile follows the base system from
[dme86/gentoo-ansible](https://github.com/dme86/gentoo-ansible):

| Component | Default |
| --- | --- |
| Architecture | Native amd64 or arm64 |
| Init | systemd stage3 |
| Partitioning | GPT, 512 MiB FAT32 ESP, optional 4 GiB swap, remaining space for root |
| Subvolumes | `@` mounted at `/`, `@home` at `/home` |
| Filesystem | Btrfs with zstd compression (default) or ext4 |
| Kernel | `gentoo-kernel-bin` (default) or `gentoo-kernel` (source build) |
| Bootloader | systemd-boot (default) or GRUB, UEFI |
| Networking | systemd-networkd, DHCP on wired interfaces |
| Package management | Portage, binary packages preferred by default, source-only optional |
| Console and locale | Configurable keymap; en_US.UTF-8 (default), de_DE.UTF-8 or C.UTF-8 |

Stagehand detects its **native live-system architecture** at runtime (`x86_64` → `amd64`, `aarch64` → `arm64`) and downloads only the corresponding systemd stage3. No Gentoo ISO download is necessary: boot a Linux live system first. Cross-architecture installations are intentionally unsupported.

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
- **System** — hostname, timezone, keymap and UTF-8 locale.
- **Kernel & boot** — `gentoo-kernel-bin` or `gentoo-kernel`; systemd-boot or GRUB (UEFI).
- **Network** — wired DHCP, optional key-only SSH and public key.
- **Users** — administrator name/password and root password.
- **Portage** — binary packages preferred or builds from source.
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

The progress screen remains open during installation. Individual commands and
Portage build output can be reviewed in the installation log.

## Current scope and next milestones

This is a **first implementation**, not a fully validated general-purpose
Gentoo installer. In particular:

- UEFI only; no BIOS/legacy boot.
- Whole-disk installs only; no partition preservation, LUKS or dual-boot.
- Wired DHCP only; Wi-Fi and advanced network setup are not yet configured.
- systemd/UEFI only; Btrfs/ext4, kernel, swap and UEFI bootloader are selectable.
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

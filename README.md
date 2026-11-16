<p align="center">
  <img src="assets/logo.svg" alt="Stagehand — a gentoo installer" width="320">
</p>

<h1 align="center">Stagehand</h1>
<p align="center"><strong>Gentoo, one step at a time.</strong></p>

A keyboard-first, guided **Gentoo Linux installer written in Rust**. Inspired by
[archinstall](https://github.com/archlinux/archinstall) and [YaST](https://yast.opensuse.org/),
Stagehand lets you configure an installation in a clean terminal interface,
review every decision, and install only after explicit confirmation.

> **Early development (v0.1.0).** The installer has not yet passed an end-to-end
> UEFI VM boot test. **Do not run it against a disk containing valuable data.**
> Use a disposable virtual machine until the installation and boot path are validated.

## One screen, all the important choices

```text
┌ Welcome ───────────────────────────────────────────────────────────────────┐
│ STAGEHAND                  Gentoo installer · v0.1.0                       │
├ Installation settings ───────┬ Details ────────────────────────────────────┤
│  Storage          /dev/vda    │ Welcome to Stagehand.                      │
│  Hostname         gentoo      │                                             │
│  Timezone         Europe/...  │ • Gentoo Linux with systemd                │
│  Keymap           de          │ • GPT / UEFI / systemd-boot                 │
│  User             dan         │ • Btrfs @ and @home (zstd)                  │
│  User password    Configured  │ • Gentoo binary kernel                     │
│  Root password    Configured  │ • systemd-networkd / DHCP                  │
│  SSH service      Disabled    │ • Native amd64 and arm64                   │
│  SSH public key   Not set     │                                             │
│ ▶ Install Gentoo  Review     │ No disk changes until final confirmation.  │
├ Keys ────────────────────────┴─────────────────────────────────────────────┤
│ ↑↓/j/k Navigate   Enter Edit   q Quit                                      │
└────────────────────────────────────────────────────────────────────────────┘
```

The interface uses **ratatui + crossterm** with a compact Gruvbox-inspired color
scheme. It runs entirely in the terminal and does not require Python, Ansible,
a graphical environment, or a desktop environment.

### What you configure

- Target disk, selected from eligible unmounted disks.
- Hostname, timezone and console keymap.
- Local administrator account and root credentials, with password confirmation.
- Optional SSH with public-key authentication (password SSH and root SSH disabled).
- Final installation review and a mandatory typed `ERASE /dev/...` confirmation.

### What Stagehand installs

The first install profile deliberately follows the base system from
[dme86/gentoo-ansible](https://github.com/dme86/gentoo-ansible):

| Component | Default |
| --- | --- |
| Architecture | Native amd64 or arm64 |
| Init | systemd stage3 |
| Partitioning | GPT, 512 MiB FAT32 ESP, remaining space Btrfs |
| Subvolumes | `@` mounted at `/`, `@home` at `/home` |
| Filesystem | Btrfs with zstd compression |
| Kernel | `sys-kernel/gentoo-kernel-bin` |
| Bootloader | systemd-boot, UEFI |
| Networking | systemd-networkd, DHCP on wired interfaces |
| Package management | Portage, binary packages preferred |
| Console | Configurable keymap; English base system messages |

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
`btrfs`, `mount`, `umount`, `chroot`, `tar`, `curl`, and `sync`.
The chosen disk must have at least 8 GiB; a larger disk is strongly
recommended for Portage. Stagehand refuses mounted, read-only and removable
target disks, and checks active swap.

**First try Stagehand inside a disposable UEFI QEMU/UTM virtual machine**, with
a dedicated, empty virtual disk. Do not boot your everyday Linux system and
point the installer at an attached physical drive.

## Keyboard controls

| Key | Action |
| --- | --- |
| `↑` / `↓`, `j` / `k` | Navigate settings |
| `Enter` | Select, edit, or save |
| `Esc` | Return to main menu |
| `r` | Refresh disk list |
| `q` | Quit from main menu |

The progress screen remains open during installation. Individual commands and
Portage build output can be reviewed in the installation log.

## Current scope and next milestones

This is a **first implementation**, not a fully validated general-purpose
Gentoo installer. In particular:

- UEFI only; no BIOS/legacy boot.
- Whole-disk installs only; no partition preservation, LUKS or dual-boot.
- Wired DHCP only; Wi-Fi and advanced network setup are not yet configured.
- Fixed systemd + Btrfs + binary kernel profile (other profiles planned).
- No automated integration test booting the resulting disk yet.
- Re-running the installation is destructive; no resume support.
- SHA-256 validation protects downloaded archives against accidental corruption,
  but signed Gentoo OpenPGP verification is not yet implemented.
- Kernel and bootloader combinations need to be verified on real UEFI VMs.

Roadmap: automated QEMU boot testing, signed artifact verification, support for
filesystem/boot profile choices, locale and network configuration, package
selection, disk encryption and optional Anvil + rst desktop integration.

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

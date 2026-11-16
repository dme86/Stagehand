# Gentoo installation sequence (Stagehand)

Audited against the [Gentoo AMD64 Handbook](https://wiki.gentoo.org/wiki/Handbook:AMD64),
especially *The stage file*, *Installing base system*, *Configuring the kernel*,
*Configuring the system*, *Configuring the bootloader*, and *Finalizing*, and
[dme86/gentoo-ansible/install.yml](https://github.com/dme86/gentoo-ansible/blob/main/install.yml).

This document describes the actual ordered steps in `src/install.rs`, not a
recipe to invoke manually. The installer runs independently of the ratatui UI
in a worker process after the typed disk erase confirmation.

| Phase | Context | Actions / ordering invariant |
| --- | --- | --- |
| 1. Preflight | Live ISO | Validate root, UEFI, architecture, dependencies, selected device, swap usage, and empty mount target |
| 2. Fetch Stage3 | Live ISO | Select the systemd base/desktop Stage3 for native amd64/arm64; download and verify its SHA-256 **before any disk writes** |
| 3. Partition | Live ISO | Recheck the disk state just before the destructive boundary; create GPT, ESP, root, and optionally swap |
| 4. Format/mount | Live ISO | Format partitions, mount root and Btrfs subvolumes if selected, mount ESP on `/mnt/stagehand/boot` **before installing any kernel** |
| 5. Extract Stage3 | Live ISO | Extract with tar permissions, xattrs and numeric ownership preserved |
| 6. Initial configuration | Live ISO, writing into installed root | Create `fstab`, `hostname`, `hosts`, timezone, locale, console keymap, network config, `/etc/kernel/cmdline`, installkernel USE flags, firmware license and Portage settings; copy live DNS resolver for chroot |
| 7. Prepare chroot | Live ISO | Mount proc, rbind sys and dev with slave propagation, bind run; verify **all required mountpoints** (including root, ESP and Btrfs home), Stage3 identity, `fstab`, Portage config and DNS; run **`env-update` inside chroot** |
| 8. Repository | Gentoo chroot, login shell | Source **`/etc/profile`** (after env-update); run `emerge-webrsync` |
| 9. Packages/kernel | Gentoo chroot, login shell | **First update `sys-apps/systemd` with the `boot` USE flag for systemd-boot**, then install/update `sys-kernel/installkernel` and GRUB (if selected) with `--newuse`; **only then** install Btrfs tools in a dedicated transaction when selected; install Dracut and firmware; verify /usr/bin/btrfs in the target chroot for Btrfs roots; install the distribution kernel separately; validate the initramfs with lsinitrd; generate locales |
| 10. Users | Gentoo chroot | Set credentials through stdin, create administrator, validate sudoers, optionally configure key-only SSH |
| 11. Services/boot | Gentoo chroot | Enable selected system services; switch installed resolver to systemd-resolved; **recheck ESP mounts**; install EFI bootloader and verify BLS default, kernel/initramfs/root parameters, architecture-correct EFI files, or GRUB's default kernel entry against the real root UUID; GRUB Btrfs roots require `rootflags=subvol=@` |
| 12. Finish | Live ISO | Sync, recursively unmount chroot and installed filesystems; worker records final state |

## Why these boundaries matter

- The **live environment** must create and mount filesystems before extracting
  Stage3; `fstab` describes how those filesystems will mount **on next boot**.
- Stage3 owns `/etc/portage` and `/etc/profile`, so their configuration must
  be written **after extraction** and **before Portage runs**.
- `/proc`, `/sys`, `/dev`, `/run` and a usable `/etc/resolv.conf` are
  needed inside the chroot before synchronizing or installing packages.
- The `installkernel` and systemd boot USE flags, `/etc/kernel/cmdline`, and `/etc/dracut.conf.d/90-stagehand.conf` must exist
  **before** the distribution kernel is emerged. Rebuilding `systemd[boot]`
  and installing `installkernel`
  in separate transactions avoids relying on the ordering of one big package
  transaction. This is particularly important for systemd-boot.
- GRUB does **not** take its command line from `/etc/kernel/cmdline`;
  with root on Btrfs `@`, its generated entry must include
  `rootflags=subvol=@`. On amd64, GRUB is built with
  `GRUB_PLATFORMS="efi-64"`.
- A generated config or a compiled Rust binary does not prove the system will
  boot. Stagehand detects missing mountpoints and invalid bootloader configs,
  but end-to-end UEFI QEMU tests are still needed.


### Generating Dracut initramfs inside the chroot

Stagehand writes `hostonly="no"` and `hostonly_cmdline="no"` into
`/etc/dracut.conf.d/90-stagehand.conf` **before** kernel installation.
The installkernel hook runs inside the target Gentoo chroot and cannot
accidentally embed the live ISO's kernel command line. Dracut, Linux
firmware and selected filesystem utilities are installed first.

The generated image is checked against the BLS entry (systemd-boot) or
a matching kernel version (GRUB), then inspected by `lsinitrd -m` in the
Gentoo chroot. Any missing or unreadable initramfs stops installation.


### Bootloader chain validation (0.5.3)

Before emerging the kernel, Stagehand generates an independent random
machine ID and a public `gentoo-<random>` entry token for systemd-boot.
It explicitly configures `layout=bls`. After installing
the bootloader, the validator checks the configured default BLS entry,
its kernel/initrd resources, exact kernel command line, the fallback EFI
binary, and the systemd EFI loader. `bootctl list` runs inside the chroot.

GRUB validates `grub.cfg` syntax and its **default** Linux entry, including
real kernel and matching-version initramfs files plus root= UUID identity
from the installed root partition. Both paths check EFI PE machine type
(AMD64 vs ARM64) and reject missing or suspicious boot resources.


### Unique machine identity and stable boot entry names (0.5.4)

Stagehand writes an independent 128-bit systemd machine ID to the new
Gentoo installation **before** Portage runs. This ID is never copied from
the live ISO and remains stable across the first boot and subsequent boots.

For systemd-boot, a separate random 96-bit token prefixed with `gentoo-`
is written to `/etc/kernel/entry-token`, preventing installation name
collisions and avoiding machine ID disclosure in FAT EFI filenames.
`kernel-install`, `bootctl --entry-token=literal:<token>`, and the
`default <token>-*` loader pattern all use the *same* token. The boot
validator rejects inconsistent or missing values. Both values are created
before the kernel triggers installkernel; they are not regenerated later.

Preinitializing machine-id means `ConditionFirstBoot=yes` is not triggered.
That is intentional for this interactive installer, which provisions users,
locale, networking and services itself (not a reusable disk image).

### Btrfs tool ordering and pre-kernel verification (v0.5.5)

For Btrfs roots, Stagehand installs `sys-fs/btrfs-progs` in its **own**
Portage invocation, **before** Dracut, Linux firmware and the kernel.
Before emerging the selected Gentoo kernel it runs `/usr/bin/btrfs --version`
**inside the target Gentoo chroot**, so a missing or unusable installed
Btrfs tool stops the install without creating a questionable initramfs.
For ext4 roots these steps are skipped. The package plan and check plan are
unit-tested for both GRUB and systemd-boot.

## Validation

The CI includes unit tests for documented phase order, required chroot
mountpoints (including Btrfs home), and bootloader configuration checks.
These are **non-destructive** tests; they do not replace a real installation
and first boot in QEMU/UTM.

### Scope

The reference Ansible playbook is specifically for an amd64/arm64 QEMU guest
with a Btrfs systemd setup. Stagehand adds ext4, optional swap, selectable
kernel/UEFI bootloader, alternative network manager and other menus; those
paths need independent VM test coverage.

use crate::{
    boot_verify,
    config::{Bootloader, Config, Filesystem, Locale, Network, Stage3, Swap},
    disks,
};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::symlink, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const LOG: &str = "/tmp/stagehand-install.log";
pub const STEP_COUNT: usize = 12;
const ROOT: &str = "/mnt/stagehand";
const MIRROR: &str = "https://distfiles.gentoo.org";

// Dracut 106+ defaults to host-only mode when invoked by installkernel.
// During a live ISO installation that may detect the *installer* hardware
// instead of the system we are building. Never embed the live ISO cmdline.
const DRACUT_CONFIG: &str = "\
# Stagehand: generic initramfs for the installed Gentoo system.\n\
# Never use the live ISO's devices or root= command line.\n\
hostonly=\"no\"\n\
hostonly_cmdline=\"no\"\n";

/// Stable phase sequence for the TUI. The gauge measures completed phases,
/// never elapsed time or estimated compilation/download progress.
pub const PHASES: [&str; 12] = [
    "Preflight checks",
    "Fetch and verify Gentoo stage3",
    "Partition selected disk",
    "Format and mount selected filesystem",
    "Extract systemd stage3",
    "Configure filesystem, locale and network",
    "Prepare Gentoo chroot",
    "Initialize Portage tree",
    "Install kernel, boot and essential packages",
    "Create accounts and credentials",
    "Enable services and configure bootloader",
    "Sync disks and safely unmount",
];

pub fn run(c: &Config, report: &mut impl FnMut(usize, &str) -> Result<()>) -> Result<()> {
    install(c, report)
}
fn progress(
    report: &mut impl FnMut(usize, &str) -> Result<()>,
    n: usize,
    title: &str,
) -> Result<()> {
    report(n, title)
}
fn install(c: &Config, report: &mut impl FnMut(usize, &str) -> Result<()>) -> Result<()> {
    File::create(LOG)?;
    progress(report, 1, "Preflight checks")?;
    c.validate().map_err(anyhow::Error::msg)?;
    preflight(c)?;

    progress(report, 2, "Fetch and verify Gentoo stage3")?;
    let stage3 = fetch_stage3(arch()?, c.stage3)?;

    // This is the destructive boundary: all earlier steps are read-only.
    progress(report, 3, "Partition selected disk")?;
    let selected = c.disk.as_ref().context("No disk selected")?;
    disks::verify_target(selected)?;
    let disk = selected.path.as_str();
    let mut plan = vec![
        "-s", disk, "mklabel", "gpt", "mkpart", "ESP", "fat32", "1MiB", "513MiB", "set", "1",
        "esp", "on",
    ];
    if c.swap == Swap::FourGiB {
        plan.extend_from_slice(&["mkpart", "SWAP", "linux-swap", "513MiB", "4609MiB"]);
    }
    let root_number = if c.swap == Swap::FourGiB { 3 } else { 2 };
    let root_start = if c.swap == Swap::FourGiB {
        "4609MiB"
    } else {
        "513MiB"
    };
    let kind = match c.filesystem {
        Filesystem::Btrfs => "btrfs",
        Filesystem::Ext4 => "ext4",
    };
    plan.extend_from_slice(&["mkpart", "ROOT", kind, root_start, "100%"]);
    exec("parted", &plan)?;
    exec("udevadm", &["settle"])?;
    let esp = disks::partition_path(disk, 1);
    let root = disks::partition_path(disk, root_number);
    let swap_device = (c.swap == Swap::FourGiB).then(|| disks::partition_path(disk, 2));

    progress(report, 4, "Format and mount selected filesystem")?;
    exec("mkfs.vfat", &["-F", "32", "-n", "EFI", &esp])?;
    if let Some(device) = &swap_device {
        exec("mkswap", &["-L", "SWAP", device])?;
    }
    fs::create_dir_all(ROOT)?;
    let mut guard = MountGuard(false);
    match c.filesystem {
        Filesystem::Btrfs => {
            exec("mkfs.btrfs", &["-f", "-L", "ROOT", &root])?;
            exec("mount", &[&root, ROOT])?;
            guard.0 = true;
            exec("btrfs", &["subvolume", "create", &format!("{ROOT}/@")])?;
            exec("btrfs", &["subvolume", "create", &format!("{ROOT}/@home")])?;
            exec("umount", &[ROOT])?;
            guard.0 = false;
            exec(
                "mount",
                &[
                    "-o",
                    "noatime,compress=zstd,space_cache=v2,subvol=@",
                    &root,
                    ROOT,
                ],
            )?;
            guard.0 = true;
            fs::create_dir_all(format!("{ROOT}/home"))?;
            exec(
                "mount",
                &[
                    "-o",
                    "noatime,compress=zstd,space_cache=v2,subvol=@home",
                    &root,
                    &format!("{ROOT}/home"),
                ],
            )?;
        }
        Filesystem::Ext4 => {
            exec("mkfs.ext4", &["-F", "-L", "ROOT", &root])?;
            exec("mount", &[&root, ROOT])?;
            guard.0 = true;
        }
    }
    fs::create_dir_all(format!("{ROOT}/boot"))?;
    exec("mount", &[&esp, &format!("{ROOT}/boot")])?;

    progress(report, 5, "Extract systemd stage3")?;
    exec(
        "tar",
        &[
            "xpf",
            stage3.to_str().context("Invalid stage3 path")?,
            "--xattrs-include=*.*",
            "--numeric-owner",
            "-C",
            ROOT,
        ],
    )?;

    progress(report, 6, "Configure filesystem, locale and network")?;
    configure(c)?;

    progress(report, 7, "Prepare Gentoo chroot")?;
    for part in ["proc", "sys", "dev", "run"] {
        fs::create_dir_all(format!("{ROOT}/{part}"))?;
    }
    exec("mount", &["-t", "proc", "proc", &format!("{ROOT}/proc")])?;
    exec("mount", &["--rbind", "/sys", &format!("{ROOT}/sys")])?;
    exec("mount", &["--make-rslave", &format!("{ROOT}/sys")])?;
    exec("mount", &["--rbind", "/dev", &format!("{ROOT}/dev")])?;
    exec("mount", &["--make-rslave", &format!("{ROOT}/dev")])?;
    exec("mount", &["--bind", "/run", &format!("{ROOT}/run")])?;
    // The Gentoo Handbook requires /proc, /sys, /dev and /run to be
    // mounted before chroot. Check that they really are mountpoints.
    verify_chroot_ready(&fs::read_to_string("/proc/self/mountinfo")?, c)?;

    // env-update writes /etc/profile.env and regenerates ld.so.cache.
    // A profiled shell reads it inside the chroot, not from the live ISO.
    inside(&["/usr/sbin/env-update"])?;

    progress(report, 8, "Initialize Portage tree")?;
    inside_profiled(&["/usr/bin/emerge-webrsync"])?;

    progress(report, 9, "Install kernel, boot and essential packages")?;
    // Keep the prerequisite transactions distinct and ordered.
    for command in boot_prerequisite_commands(c) {
        inside_profiled(&command)?;
    }
    // The EFI partition is already mounted at /boot, and the Stagehand
    // dracut configuration and root= cmdline are in place. Install the
    // kernel separately so installkernel generates its initramfs now.
    inside_profiled(&["/usr/bin/emerge", "--noreplace", c.kernel.package()])?;
    verify_generated_initramfs(c.bootloader)?;

    let mut packages = vec![
        "/usr/bin/emerge",
        "--noreplace",
        "app-admin/sudo",
        "sys-apps/locale-gen",
    ];
    if c.ssh_enabled {
        packages.push("net-misc/openssh");
    }
    if c.network == Network::NetworkManager {
        packages.push("net-misc/networkmanager");
    }
    if c.cronie {
        packages.push("sys-process/cronie");
    }
    if let Some(editor) = c.editor.package() {
        packages.push(editor);
    }
    inside_profiled(&packages)?;
    inside_profiled(&["/usr/bin/env", "locale-gen"])?;

    progress(report, 10, "Create accounts and credentials")?;
    inside_secret(
        &["/usr/sbin/chpasswd"],
        format!("root:{}\n", c.root_password).as_bytes(),
    )?;
    inside(&[
        "/usr/sbin/useradd",
        "-m",
        "-G",
        "wheel",
        "-s",
        "/bin/bash",
        &c.username,
    ])?;
    inside_secret(
        &["/usr/sbin/chpasswd"],
        format!("{}:{}\n", c.username, c.user_password).as_bytes(),
    )?;
    write(
        "etc/sudoers.d/stagehand-wheel",
        "%wheel ALL=(ALL:ALL) ALL\n",
    )?;
    fs::set_permissions(
        format!("{ROOT}/etc/sudoers.d/stagehand-wheel"),
        fs::Permissions::from_mode(0o440),
    )?;
    ensure_sudoers_includes()?;
    inside(&["/usr/sbin/visudo", "-cf", "/etc/sudoers"])?;
    if c.ssh_enabled {
        let ssh = format!("{ROOT}/home/{}/.ssh", c.username);
        fs::create_dir_all(&ssh)?;
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700))?;
        fs::write(
            format!("{ssh}/authorized_keys"),
            format!("{}\n", c.ssh_public_key.trim()),
        )?;
        fs::set_permissions(
            format!("{ssh}/authorized_keys"),
            fs::Permissions::from_mode(0o600),
        )?;
        inside(&[
            "/usr/bin/chown",
            "-R",
            &format!("{}:{}", c.username, c.username),
            &format!("/home/{}/.ssh", c.username),
        ])?;
        write(
            "etc/ssh/sshd_config.d/20-stagehand.conf",
            "PermitRootLogin no\nPasswordAuthentication no\nPubkeyAuthentication yes\n",
        )?;
    }

    progress(report, 11, "Enable services and configure bootloader")?;
    let mut services = vec![
        "/usr/bin/systemctl",
        "enable",
        "systemd-resolved",
        "systemd-timesyncd",
    ];
    match c.network {
        Network::Networkd => services.push("systemd-networkd"),
        Network::NetworkManager => services.push("NetworkManager"),
    }
    if c.ssh_enabled {
        services.push("sshd");
    }
    if c.cronie {
        services.push("cronie");
    }
    inside(&services)?;
    let resolver = Path::new(ROOT).join("etc/resolv.conf");
    fs::remove_file(&resolver)?;
    symlink("../run/systemd/resolve/stub-resolv.conf", &resolver)?;
    // The kernel has already been installed; now verify the ESP and produce
    // the final bootloader configuration before syncing/unmounting.
    validate_chroot_mounts(&fs::read_to_string("/proc/self/mountinfo")?, c.filesystem)?;
    install_bootloader(c)?;

    progress(report, 12, "Sync disks and safely unmount")?;
    exec("sync", &[])?;
    exec("umount", &["-R", ROOT])?;
    guard.0 = false;
    Ok(())
}
/// Required installation order: systemd[boot] (when selected), then
/// installkernel configured for the selected loader, then the kernel itself.
/// Keeping this pure makes the ordering testable without disk I/O.
fn boot_prerequisite_commands(c: &Config) -> Vec<Vec<&'static str>> {
    let mut commands = Vec::new();
    if c.bootloader == Bootloader::SystemdBoot {
        commands.push(vec![
            "/usr/bin/emerge",
            "--update",
            "--newuse",
            "--oneshot",
            "sys-apps/systemd",
        ]);
    }
    let mut installkernel = vec![
        "/usr/bin/emerge",
        "--update",
        "--newuse",
        "--oneshot",
        "sys-kernel/installkernel",
    ];
    if c.bootloader == Bootloader::Grub {
        installkernel.push("sys-boot/grub");
    }
    commands.push(installkernel);
    // These must be installed before the kernel's post-install hook runs:
    // the Dracut plugin, firmware, and (for Btrfs roots) btrfs tools.
    let mut prerequisites = vec![
        "/usr/bin/emerge",
        "--noreplace",
        "sys-kernel/dracut",
        "sys-kernel/linux-firmware",
    ];
    if c.filesystem == Filesystem::Btrfs {
        prerequisites.push("sys-fs/btrfs-progs");
    }
    commands.push(prerequisites);
    commands
}

fn preflight(c: &Config) -> Result<()> {
    let uid = Command::new("id").arg("-u").output()?;
    if uid.stdout != b"0\n" {
        bail!("Please run Stagehand as root.");
    }
    if !Path::new("/sys/firmware/efi").exists() {
        bail!("UEFI boot is required.");
    }
    arch()?;
    for command in [
        "lsblk",
        "blkid",
        "swapon",
        "parted",
        "udevadm",
        "mkfs.vfat",
        "mkfs.btrfs",
        "btrfs",
        "mount",
        "umount",
        "chroot",
        "tar",
        "curl",
        "sync",
    ] {
        if command == "mkfs.btrfs" && c.filesystem == Filesystem::Ext4 {
            continue;
        }
        if command == "btrfs" && c.filesystem == Filesystem::Ext4 {
            continue;
        }
        if !Command::new("sh")
            .args(["-c", "command -v \"$1\" >/dev/null", "sh", command])
            .status()?
            .success()
        {
            bail!("Missing live-system dependency: {command}");
        }
    }
    if c.filesystem == Filesystem::Ext4 {
        ensure_live_tool("mkfs.ext4")?;
    }
    if c.swap == Swap::FourGiB {
        ensure_live_tool("mkswap")?;
    }
    disks::verify_target(c.disk.as_ref().context("Select a disk")?)?;
    if Path::new(ROOT).exists() && fs::read_dir(ROOT)?.next().is_some() {
        bail!("{ROOT} is not empty. Resolve old mounts first.");
    }
    Ok(())
}
fn ensure_live_tool(name: &str) -> Result<()> {
    if !Command::new("sh")
        .args(["-c", "command -v \"$1\" >/dev/null 2>&1", "sh", name])
        .status()?
        .success()
    {
        bail!("Missing live-system dependency: {name}");
    }
    Ok(())
}
pub fn arch() -> Result<&'static str> {
    let out = Command::new("uname")
        .arg("-m")
        .output()
        .context("Detect running Linux architecture")?;
    if !out.status.success() {
        bail!("Cannot read live-system architecture from uname");
    }
    let actual = String::from_utf8(out.stdout)?;
    let detected = match actual.trim() {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        other => bail!("Unsupported live system architecture: {other}"),
    };
    let binary = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("Stagehand executable architecture is unsupported: {other}"),
    };
    if detected != binary {
        bail!("Stagehand binary and Linux live system architectures differ; cross-architecture installs are not supported.");
    }
    Ok(detected)
}
fn fetch_stage3(a: &str, flavor: Stage3) -> Result<PathBuf> {
    let prefix = flavor.archive_prefix(a);
    let base = format!("{MIRROR}/releases/{a}/autobuilds/current-{prefix}");
    let index = output(
        "curl",
        &[
            "-fsSL",
            "--retry",
            "3",
            &format!("{base}/latest-{prefix}.txt"),
        ],
    )?;
    let name = parse_name(&index, &prefix)?;
    let checksum = output(
        "curl",
        &["-fsSL", "--retry", "3", &format!("{base}/{name}.sha256")],
    )?;
    let expected = digest_for(&checksum, &name)?;
    let cache = Path::new("/tmp/stagehand-cache");
    fs::create_dir_all(cache)?;
    let file = cache.join(&name);
    if !file.exists() || sha256(&file)? != expected {
        let temp = cache.join(format!("{name}.part"));
        exec(
            "curl",
            &[
                "-fL",
                "--silent",
                "--show-error",
                "--retry",
                "3",
                "-o",
                temp.to_str().context("Invalid filename")?,
                &format!("{base}/{name}"),
            ],
        )?;
        fs::rename(temp, &file)?;
    }
    if sha256(&file)? != expected {
        bail!("Stage3 SHA-256 verification failed. No disk has been modified.");
    }
    Ok(file)
}
fn parse_name(index: &str, prefix: &str) -> Result<String> {
    index
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let size = fields.next()?;
            if fields.next().is_some()
                || size.parse::<u64>().is_err()
                || !name.starts_with(&format!("{prefix}-"))
                || !(name.ends_with(".tar.xz") || name.ends_with(".tar.zst"))
                || name.contains('/')
            {
                return None;
            }
            Some(name.to_owned())
        })
        .next()
        .ok_or_else(|| anyhow!("Stage3 manifest contains no supported archive"))
}
fn digest_for(document: &str, filename: &str) -> Result<String> {
    for line in document.lines() {
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() >= 2
            && parts[0].len() == 64
            && parts[0].bytes().all(|c| c.is_ascii_hexdigit())
            && (parts[1] == filename
                || parts[1] == format!("*{filename}")
                || parts[1] == format!("./{filename}"))
        {
            return Ok(parts[0].to_ascii_lowercase());
        }
    }
    bail!("Missing SHA-256 checksum for {filename}")
}
fn sha256(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut sha = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
/// Sudo only uses drop-ins when its main config contains the includedir directive.
fn ensure_sudoers_includes() -> Result<()> {
    let path = Path::new(ROOT).join("etc/sudoers");
    let current = fs::read_to_string(&path).context("Read installed sudoers")?;
    let has_include = current.lines().any(|line| {
        matches!(
            line.trim(),
            "@includedir /etc/sudoers.d" | "#includedir /etc/sudoers.d"
        )
    });
    if !has_include {
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"\n@includedir /etc/sudoers.d\n")?;
    }
    Ok(())
}

/// GRUB must actually reference a Linux kernel, not just exist as a file.
/// Btrfs @ must be included so Linux boots the right root subvolume.
/// Dracut must be invoked by installkernel inside the target chroot, not
/// against the live ISO kernel. The selected loader dictates where Gentoo's
/// installkernel places the initramfs.
fn verify_generated_initramfs(loader: Bootloader) -> Result<()> {
    let dracut = fs::read_to_string(Path::new(ROOT).join("etc/dracut.conf.d/90-stagehand.conf"))
        .context("Missing Stagehand Dracut configuration before kernel validation")?;
    if dracut != DRACUT_CONFIG {
        bail!("Dracut configuration changed during the install; refusing to accept an unverified initramfs");
    }
    let candidates = initramfs_candidates(Path::new(ROOT), loader)?;
    if candidates.is_empty() {
        bail!(
            "No initramfs created by installkernel for {}. Check Portage and Dracut logs in {LOG}",
            loader.label()
        );
    }
    for path in candidates {
        // This parses and decompresses the installed image using Dracut's
        // own lsinitrd, inside the new Gentoo filesystem. It must succeed.
        inside(&["/usr/bin/env", "lsinitrd", "-m", &path])
            .with_context(|| format!("Generated initramfs {path} failed lsinitrd validation"))?;
    }
    Ok(())
}

/// Reject absolute paths outside the ESP and traversal in Boot Loader Spec
/// entries; produce a chroot-visible path to the actual filesystem object.
fn boot_entry_file(root: &Path, spec: &str) -> Result<String> {
    use std::path::Component;
    let relative = spec
        .strip_prefix('/')
        .context("Boot entry reference must be an absolute path within the EFI system partition")?;
    if relative.is_empty()
        || !Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        bail!("Unsafe boot entry path {spec}");
    }
    let resolved = root.join("boot").join(relative);
    require_boot_file(&resolved)?;
    Ok(format!("/boot/{relative}"))
}

fn require_boot_file(path: &Path) -> Result<()> {
    let meta = fs::metadata(path)
        .with_context(|| format!("Missing installed boot artifact {}", path.display()))?;
    if !meta.is_file() || meta.len() < 4096 {
        bail!(
            "Boot artifact {} is missing, not a file, or implausibly small",
            path.display()
        );
    }
    Ok(())
}

/// Extract the actual BLS initrd referenced by the kernel entry, rather
/// than accepting an unrelated .img file anywhere under the ESP.
fn initramfs_candidates(root: &Path, loader: Bootloader) -> Result<Vec<String>> {
    let boot = root.join("boot");
    let mut images = Vec::new();
    match loader {
        Bootloader::SystemdBoot => {
            let entries = boot.join("loader/entries");
            for file in fs::read_dir(&entries)
                .with_context(|| format!("No systemd-boot entries under {}", entries.display()))?
            {
                let file = file?;
                if file
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "conf")
                {
                    continue;
                }
                let content = fs::read_to_string(file.path())?;
                let mut kernel = None;
                let mut initrd = None;
                for line in content.lines() {
                    let mut fields = line.split_whitespace();
                    match fields.next() {
                        Some("linux") => kernel = fields.next(),
                        Some("initrd") => initrd = fields.next(),
                        _ => {}
                    }
                }
                let Some(initrd) = initrd else { continue };
                let linux = kernel.context("systemd-boot entry has initrd but no linux image")?;
                boot_entry_file(root, linux)?;
                images.push(boot_entry_file(root, initrd)?);
            }
        }
        Bootloader::Grub => {
            for entry in fs::read_dir(&boot)? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Some(version) = name
                    .strip_prefix("initramfs-")
                    .and_then(|v| v.strip_suffix(".img"))
                else {
                    continue;
                };
                if version.is_empty() {
                    continue;
                }
                require_boot_file(&entry.path())?;
                // Both traditional and systemd-based installkernel variants
                // have used these kernel-image prefixes.
                let has_kernel = ["vmlinuz-", "kernel-"].iter().any(|prefix| {
                    require_boot_file(&boot.join(format!("{prefix}{version}"))).is_ok()
                });
                if !has_kernel {
                    bail!("Initramfs {name} has no matching installed Linux kernel image");
                }
                images.push(format!("/boot/{name}"));
            }
        }
    }
    if images.is_empty() {
        bail!("No installkernel-generated initramfs with a matching Linux kernel found on /boot");
    }
    images.sort();
    images.dedup();
    Ok(images)
}

fn install_bootloader(config: &Config) -> Result<()> {
    match config.bootloader {
        Bootloader::SystemdBoot => {
            inside(&[
                "/usr/bin/bootctl",
                "--esp-path=/boot",
                "--entry-token=literal:gentoo",
                "install",
                "--no-variables",
            ])?;
            write(
                "boot/loader/loader.conf",
                "default gentoo-*\ntimeout 3\neditor no\n",
            )?;
            // Inspect the *default-selected* BLS entries, referenced
            // kernel+initramfs, root cmdline and actual EFI loader binaries.
            boot_verify::verify_systemd_entries(Path::new(ROOT), config.filesystem)?;
            boot_verify::verify_efi_binary(Path::new(ROOT), arch()?, true)?;
            // Also ask systemd itself to discover the installed entries,
            // independent of which live ISO bootloader is currently active.
            inside(&["/usr/bin/bootctl", "--esp-path=/boot", "--no-pager", "list"])?;
        }
        Bootloader::Grub => {
            // GRUB doesn't consume /etc/kernel/cmdline. The root subvolume
            // must be included in its own generated kernel command line.
            if config.filesystem == Filesystem::Btrfs {
                let path = Path::new(ROOT).join("etc/default/grub");
                if !path.is_file() {
                    bail!("GRUB defaults missing after package installation");
                }
                OpenOptions::new().append(true).open(&path)?
                    .write_all(b"\n# Stagehand: Btrfs root subvolume\nGRUB_CMDLINE_LINUX=\"rootflags=subvol=@\"\n")?;
            }
            let target = match std::env::consts::ARCH {
                "x86_64" => "x86_64-efi",
                "aarch64" => "arm64-efi",
                _ => bail!("Unsupported GRUB target architecture"),
            };
            inside(&[
                "/usr/sbin/grub-install",
                &format!("--target={target}"),
                "--efi-directory=/boot",
                "--bootloader-id=Gentoo",
                "--removable",
                "--no-nvram",
            ])?;
            inside(&["/usr/sbin/grub-mkconfig", "-o", "/boot/grub/grub.cfg"])?;
            // Validate GRUB syntax, then its *default menuentry* against
            // this disk's filesystem UUID, kernel, initramfs and Btrfs flags.
            inside(&["/usr/bin/grub-script-check", "/boot/grub/grub.cfg"])?;
            let disk = config.disk.as_ref().context("Missing selected boot disk")?;
            let root_number = if config.swap == Swap::FourGiB { 3 } else { 2 };
            let root_partition = disks::partition_path(&disk.path, root_number);
            let uuid = output("blkid", &["-s", "UUID", "-o", "value", &root_partition])?;
            let uuid = uuid.trim();
            if uuid.is_empty() {
                bail!("Unable to read UUID of installed root partition");
            }
            boot_verify::verify_grub_entries(Path::new(ROOT), config.filesystem, uuid)?;
            boot_verify::verify_efi_binary(Path::new(ROOT), arch()?, false)?;
        }
    }
    Ok(())
}

fn configure(c: &Config) -> Result<()> {
    let mut fstab = String::from("# Managed by Stagehand\n");
    let cmdline = match c.filesystem {
        Filesystem::Btrfs => {
            fstab
                .push_str("LABEL=ROOT / btrfs noatime,compress=zstd,space_cache=v2,subvol=@ 0 0\n");
            fstab.push_str(
                "LABEL=ROOT /home btrfs noatime,compress=zstd,space_cache=v2,subvol=@home 0 0\n",
            );
            "console=tty0 root=LABEL=ROOT rootfstype=btrfs rootflags=subvol=@ rw\n"
        }
        Filesystem::Ext4 => {
            fstab.push_str("LABEL=ROOT / ext4 noatime 0 1\n");
            "console=tty0 root=LABEL=ROOT rootfstype=ext4 rw\n"
        }
    };
    fstab.push_str("LABEL=EFI /boot vfat defaults,noatime 0 2\n");
    if c.swap == Swap::FourGiB {
        fstab.push_str("LABEL=SWAP none swap sw 0 0\n");
    }
    write("etc/fstab", &fstab)?;
    write("etc/kernel/cmdline", cmdline)?;
    if c.bootloader == Bootloader::SystemdBoot {
        // Keep the loader default and kernel-install's BLS entries in sync,
        // even when /etc/machine-id is generated only on first boot.
        write("etc/kernel/entry-token", "gentoo\n")?;
        write("etc/kernel/install.conf", "layout=bls\n")?;
    }
    // Dracut reads /etc/dracut.conf.d during kernel installation inside
    // the chroot. Write this BEFORE Portage installs gentoo-kernel(-bin).
    write("etc/dracut.conf.d/90-stagehand.conf", DRACUT_CONFIG)?;
    write("etc/hostname", &format!("{}\n", c.hostname))?;
    write(
        "etc/hosts",
        &format!(
            "127.0.0.1 localhost {}\n::1 localhost {}\n",
            c.hostname, c.hostname
        ),
    )?;
    let zone = Path::new(ROOT).join("usr/share/zoneinfo").join(&c.timezone);
    if !zone.is_file() {
        bail!("Invalid or unavailable timezone {}", c.timezone);
    }
    let localtime = Path::new(ROOT).join("etc/localtime");
    if localtime.exists() || localtime.is_symlink() {
        fs::remove_file(&localtime)?;
    }
    symlink(format!("/usr/share/zoneinfo/{}", c.timezone), &localtime)?;
    write("etc/vconsole.conf", &format!("KEYMAP={}\n", c.keymap))?;
    write("etc/locale.conf", &format!("LANG={}\n", c.locale.label()))?;
    write(
        "etc/env.d/02locale",
        &format!("LANG=\"{}\"\n", c.locale.label()),
    )?;
    let locale_gen = match c.locale {
        Locale::English | Locale::Utf8 => "en_US.UTF-8 UTF-8\n",
        Locale::German => "en_US.UTF-8 UTF-8\nde_DE.UTF-8 UTF-8\n",
    };
    write("etc/locale.gen", locale_gen)?;
    // Do not copy the temporary live ISO machine identity into the new system.
    write("etc/machine-id", "")?;
    match c.network {
        Network::Networkd => write(
            "etc/systemd/network/20-wired.network",
            "[Match]\nName=en* eth*\n\n[Network]\nDHCP=yes\n",
        )?,
        Network::NetworkManager => {
            write(
                "etc/NetworkManager/conf.d/10-stagehand.conf",
                "[main]\ndns=systemd-resolved\n",
            )?;
        }
    }
    let boot_use = match c.bootloader {
        Bootloader::SystemdBoot => {
            "sys-apps/systemd boot\nsys-kernel/installkernel systemd systemd-boot dracut -grub -uki -ukify -generic-uki\n"
        }
        Bootloader::Grub => "sys-kernel/installkernel dracut grub -systemd -systemd-boot -uki -ukify -generic-uki\n",
    };
    let mut use_flags = boot_use.to_owned();
    // Do not select the pregenerated UKI path; Stagehand verifies a
    // conventional Dracut initramfs for both supported bootloaders.
    use_flags.push_str("sys-kernel/gentoo-kernel-bin -generic-uki\n");
    write("etc/portage/package.use/stagehand", &use_flags)?;
    write(
        "etc/portage/package.license/stagehand",
        "sys-kernel/linux-firmware linux-fw-redistributable\n",
    )?;
    let make = format!("{ROOT}/etc/portage/make.conf");
    let mut conf = OpenOptions::new().append(true).open(&make)?;
    // Preserve stage3 CHOST, USE flags, selected Gentoo profile and binhost.
    if c.binary_packages {
        conf.write_all(b"\n# Stagehand: prefer matching binary packages\nFEATURES=\"${FEATURES} getbinpkg\"\nEMERGE_DEFAULT_OPTS=\"${EMERGE_DEFAULT_OPTS} --getbinpkg=y --usepkg=y\"\nPORT_LOGDIR=\"/var/log/portage\"\n")?;
    } else {
        conf.write_all(b"\n# Stagehand: source-only builds\nFEATURES=\"${FEATURES} -getbinpkg\"\nEMERGE_DEFAULT_OPTS=\"${EMERGE_DEFAULT_OPTS} --getbinpkg=n --usepkg=n\"\nPORT_LOGDIR=\"/var/log/portage\"\n")?;
    }
    if let Some(opts) = c.build_jobs.makeopts() {
        conf.write_all(format!("MAKEOPTS=\"{opts}\"\n").as_bytes())?;
    }
    if c.bootloader == Bootloader::Grub && arch()? == "amd64" {
        // Handbook: UEFI GRUB on amd64 must be built with EFI support.
        conf.write_all(b"GRUB_PLATFORMS=\"efi-64\"\n")?;
    }
    fs::create_dir_all(format!("{ROOT}/var/log/portage"))?;
    let resolver = Path::new(ROOT).join("etc/resolv.conf");
    if resolver.exists() || resolver.is_symlink() {
        fs::remove_file(&resolver)?;
    }
    fs::copy("/etc/resolv.conf", resolver)?;
    Ok(())
}
fn write(path: &str, text: &str) -> Result<()> {
    let file = Path::new(ROOT).join(path);
    fs::create_dir_all(file.parent().context("Missing parent directory")?)?;
    fs::write(file, text).with_context(|| format!("Write {path}"))
}
/// Require all chroot pseudo-filesystems to be mounted before executing
/// Portage commands, and check ESP + Btrfs home mount placement too.
fn validate_chroot_mounts(mountinfo: &str, filesystem: Filesystem) -> Result<()> {
    let mut required = vec![
        ROOT.to_string(),
        format!("{ROOT}/boot"),
        format!("{ROOT}/proc"),
        format!("{ROOT}/sys"),
        format!("{ROOT}/dev"),
        format!("{ROOT}/run"),
    ];
    if filesystem == Filesystem::Btrfs {
        required.push(format!("{ROOT}/home"));
    }
    for mount in required {
        let present = mountinfo.lines().any(|line| {
            // /proc/self/mountinfo: mountpoint is field #5 (zero-based 4).
            line.split_whitespace().nth(4) == Some(mount.as_str())
        });
        if !present {
            bail!("Required installation mountpoint {mount} is not mounted. Refusing to enter chroot.");
        }
    }
    Ok(())
}
fn verify_chroot_ready(mountinfo: &str, config: &Config) -> Result<()> {
    for file in [
        "etc/gentoo-release",
        "etc/portage/make.conf",
        "etc/fstab",
        "etc/resolv.conf",
    ] {
        let path = Path::new(ROOT).join(file);
        if !path.is_file() {
            bail!(
                "Missing staged Gentoo configuration {}. Refusing to chroot.",
                path.display()
            );
        }
    }
    validate_chroot_mounts(mountinfo, config.filesystem)
}

/// Direct argv only: shell expansion is not used for user-provided values.
/// Clear any Portage or ROOT environment from the live ISO.
fn chroot_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("chroot");
    cmd.arg(ROOT)
        .args(args)
        .env_clear()
        .env("HOME", "/root")
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("TERM", "dumb");
    cmd
}
/// Explicitly load /etc/profile *inside* Gentoo after env-update, as
/// documented by the Handbook; "$@" passes argv without shell injection.
fn inside_profiled(args: &[&str]) -> Result<()> {
    let mut argv = vec![
        "/bin/bash",
        "-lc",
        "source /etc/profile && exec \"$@\"",
        "stagehand",
    ];
    argv.extend_from_slice(args);
    inside(&argv)
}
fn inside(args: &[&str]) -> Result<()> {
    let mut log = OpenOptions::new().append(true).create(true).open(LOG)?;
    writeln!(log, "> chroot {} {}", ROOT, args.join(" "))?;
    let status = chroot_command(args)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .status()
        .context("Execute Gentoo chroot command")?;
    if !status.success() {
        bail!("Chroot command failed ({status}); inspect {LOG}");
    }
    Ok(())
}
fn inside_secret(args: &[&str], secret: &[u8]) -> Result<()> {
    let mut log = OpenOptions::new().append(true).create(true).open(LOG)?;
    writeln!(log, "> chroot [stdin redacted] {}", args.join(" "))?;
    let mut child = chroot_command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()
        .context("Start credential setup in Gentoo chroot")?;
    child
        .stdin
        .take()
        .context("Could not pipe credentials")?
        .write_all(secret)?;
    if !child.wait()?.success() {
        bail!("Credential setup failed. See {LOG}");
    }
    Ok(())
}
fn exec(exe: &str, args: &[&str]) -> Result<()> {
    let mut log = OpenOptions::new().append(true).create(true).open(LOG)?;
    writeln!(log, "> {} {}", exe, args.join(" "))?;
    let status = Command::new(exe)
        .args(args)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .status()
        .with_context(|| format!("Run {exe}"))?;
    if !status.success() {
        bail!("{} failed ({}). Details: {}", exe, status, LOG);
    }
    Ok(())
}
fn output(exe: &str, args: &[&str]) -> Result<String> {
    let result = Command::new(exe).args(args).output()?;
    if !result.status.success() {
        bail!(
            "{} failed: {}",
            exe,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(String::from_utf8(result.stdout)?)
}
struct MountGuard(bool);
impl Drop for MountGuard {
    fn drop(&mut self) {
        if self.0 {
            let result = Command::new("umount").args(["-R", ROOT]).output();
            if let Ok(o) = result {
                if !o.status.success() {
                    if let Ok(mut log) = OpenOptions::new().append(true).open(LOG) {
                        let _ = writeln!(
                            log,
                            "WARNING: unmount failed: {}",
                            String::from_utf8_lossy(&o.stderr)
                        );
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_parsing() {
        assert_eq!(
            parse_name(
                "stage3-amd64-systemd-20261116.tar.xz 123\n",
                "stage3-amd64-systemd"
            )
            .unwrap(),
            "stage3-amd64-systemd-20261116.tar.xz"
        );
        assert!(parse_name("../bad 42", "stage3-amd64-systemd").is_err());
        assert_eq!(
            parse_name(
                "stage3-arm64-desktop-systemd-20261115T230104Z.tar.xz 100\n",
                "stage3-arm64-desktop-systemd"
            )
            .unwrap(),
            "stage3-arm64-desktop-systemd-20261115T230104Z.tar.xz"
        );
        assert!(parse_name(
            "stage3-arm64-systemd-20261115.tar.xz 100\n",
            "stage3-arm64-desktop-systemd"
        )
        .is_err());
    }
    #[test]
    fn checksum_is_bound_to_archive_name() {
        let checksum = format!("{}  another.tar.xz", "a".repeat(64));
        assert!(digest_for(&checksum, "stage3-amd64-systemd.tar.xz").is_err());
        let correct = format!("{}  stage3-amd64-systemd.tar.xz", "a".repeat(64));
        assert_eq!(
            digest_for(&correct, "stage3-amd64-systemd.tar.xz").unwrap(),
            "a".repeat(64)
        );
    }
    #[test]
    fn kernel_bootstrap_is_explicitly_ordered() {
        let mut c = Config::default();
        let systemd = boot_prerequisite_commands(&c);
        assert_eq!(systemd.len(), 3);
        assert_eq!(systemd[0].last(), Some(&"sys-apps/systemd"));
        assert_eq!(systemd[1].last(), Some(&"sys-kernel/installkernel"));
        assert!(systemd[..2].iter().all(|argv| argv.contains(&"--newuse")));
        assert!(systemd[2].contains(&"sys-kernel/dracut"));
        assert!(systemd[2].contains(&"sys-kernel/linux-firmware"));
        assert!(systemd[2].contains(&"sys-fs/btrfs-progs"));

        c.bootloader = Bootloader::Grub;
        let grub = boot_prerequisite_commands(&c);
        assert_eq!(grub.len(), 2);
        assert_eq!(grub[0].last(), Some(&"sys-boot/grub"));
        assert!(grub[1].contains(&"sys-kernel/dracut"));

        c.filesystem = Filesystem::Ext4;
        let ext4 = boot_prerequisite_commands(&c);
        assert!(!ext4[1].contains(&"sys-fs/btrfs-progs"));
    }
    #[test]
    fn installer_dracut_conf_disables_live_host_detection() {
        assert!(DRACUT_CONFIG.contains("hostonly=\"no\""));
        assert!(DRACUT_CONFIG.contains("hostonly_cmdline=\"no\""));
        assert!(!DRACUT_CONFIG.contains("hostonly=\"yes\""));
    }
    fn fake_boot_root() -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("stagehand-boot-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn systemd_boot_requires_an_initrd_and_matching_linux_file() {
        let root = fake_boot_root();
        let result = (|| -> Result<()> {
            let boot = root.join("boot");
            fs::create_dir_all(boot.join("loader/entries"))?;
            fs::create_dir_all(boot.join("gentoo/6.12"))?;
            fs::write(boot.join("gentoo/6.12/linux"), vec![1; 8192])?;
            fs::write(boot.join("gentoo/6.12/initrd"), vec![1; 8192])?;
            fs::write(boot.join("loader/entries/gentoo.conf"),
                "title Gentoo\nlinux /gentoo/6.12/linux\ninitrd /gentoo/6.12/initrd\noptions root=LABEL=ROOT rw\n")?;
            assert_eq!(
                initramfs_candidates(&root, Bootloader::SystemdBoot)?,
                vec!["/boot/gentoo/6.12/initrd"]
            );
            fs::remove_file(boot.join("gentoo/6.12/initrd"))?;
            assert!(initramfs_candidates(&root, Bootloader::SystemdBoot).is_err());
            fs::write(
                boot.join("loader/entries/gentoo.conf"),
                "linux /gentoo/6.12/linux\ninitrd /../../etc/shadow\n",
            )?;
            assert!(initramfs_candidates(&root, Bootloader::SystemdBoot).is_err());
            Ok(())
        })();
        let _ = fs::remove_dir_all(root);
        result.unwrap();
    }
    #[test]
    fn grub_initramfs_must_match_a_kernel_version() {
        let root = fake_boot_root();
        let result = (|| -> Result<()> {
            let boot = root.join("boot");
            fs::create_dir_all(&boot)?;
            fs::write(boot.join("initramfs-6.12-gentoo.img"), vec![1; 8192])?;
            assert!(initramfs_candidates(&root, Bootloader::Grub).is_err());
            fs::write(boot.join("vmlinuz-6.12-gentoo"), vec![1; 8192])?;
            assert_eq!(
                initramfs_candidates(&root, Bootloader::Grub)?,
                vec!["/boot/initramfs-6.12-gentoo.img"]
            );
            fs::write(boot.join("initramfs-6.12-gentoo.img"), vec![1; 128])?;
            assert!(initramfs_candidates(&root, Bootloader::Grub).is_err());
            Ok(())
        })();
        let _ = fs::remove_dir_all(root);
        result.unwrap();
    }
    #[test]
    fn handbook_installation_phase_order() {
        assert_eq!(PHASES.len(), STEP_COUNT);
        let position = |description: &str| {
            PHASES
                .iter()
                .position(|phase| phase.contains(description))
                .unwrap()
        };
        assert!(position("Fetch and verify") < position("Partition selected disk"));
        assert!(position("Partition selected disk") < position("Extract systemd stage3"));
        assert!(position("Extract systemd stage3") < position("Configure filesystem"));
        assert!(position("Configure filesystem") < position("Prepare Gentoo chroot"));
        assert!(position("Prepare Gentoo chroot") < position("Initialize Portage"));
        assert!(position("Initialize Portage") < position("Install kernel"));
        assert!(position("Install kernel") < position("Enable services"));
        assert!(position("Enable services") < position("Sync disks"));
    }
    #[test]
    fn chroot_requires_every_mountpoint() {
        let paths = [
            ROOT.to_string(),
            format!("{ROOT}/boot"),
            format!("{ROOT}/home"),
            format!("{ROOT}/proc"),
            format!("{ROOT}/sys"),
            format!("{ROOT}/dev"),
            format!("{ROOT}/run"),
        ];
        let mountinfo = paths
            .iter()
            .enumerate()
            .map(|(id, path)| {
                format!(
                    "{} 1 0:12 / {path} rw,relatime - ext4 /dev/vda2 rw\n",
                    id + 11
                )
            })
            .collect::<String>();
        assert!(validate_chroot_mounts(&mountinfo, Filesystem::Btrfs).is_ok());
        let missing_boot = mountinfo
            .lines()
            .filter(|line| !line.contains(&format!("{ROOT}/boot ")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(validate_chroot_mounts(&missing_boot, Filesystem::Btrfs).is_err());
        let missing_home = mountinfo
            .lines()
            .filter(|line| !line.contains(&format!("{ROOT}/home ")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(validate_chroot_mounts(&missing_home, Filesystem::Ext4).is_ok());
        assert!(validate_chroot_mounts(&missing_home, Filesystem::Btrfs).is_err());
    }
}

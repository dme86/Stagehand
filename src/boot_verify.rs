//! Validate the installed UEFI boot chain, not just the presence of a .conf.
//! Pure filesystem checks can be unit-tested without a firmware VM.
use crate::config::Filesystem;
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path},
};

const TOKEN: &str = "gentoo";
const MIN_BOOT_FILE_SIZE: u64 = 4096;

fn artifact(path: &Path) -> Result<()> {
    // A freshly formatted FAT ESP must not contain links into the live ISO.
    let meta = fs::symlink_metadata(path)
        .with_context(|| format!("Missing boot artifact {}", path.display()))?;
    if !meta.file_type().is_file() || meta.len() < MIN_BOOT_FILE_SIZE {
        bail!("Boot artifact {} is not a regular file or is too small", path.display());
    }
    Ok(())
}

fn on_esp(root: &Path, spec: &str) -> Result<()> {
    let relative = spec.strip_prefix('/')
        .context("Boot paths must be absolute paths within the ESP")?;
    if relative.is_empty()
        || !Path::new(relative).components().all(|p| matches!(p, Component::Normal(_)))
    {
        bail!("Invalid boot artifact path: {spec}");
    }
    let esp = root.join("boot");
    let mut current = esp;
    for component in Path::new(relative).components() {
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)
            .with_context(|| format!("Boot entry refers to missing file {}", current.display()))?
            .file_type().is_symlink()
        {
            bail!("Boot entry follows symlink {}", current.display());
        }
    }
    artifact(&current)
}

fn check_pe(path: &Path, arch: &str) -> Result<()> {
    artifact(path)?;
    let machine = match arch {
        "amd64" => 0x8664u16,
        "arm64" => 0xaa64u16,
        other => bail!("Unsupported EFI architecture {other}"),
    };
    let mut file = File::open(path)?;
    let mut dos = [0u8; 64];
    file.read_exact(&mut dos)?;
    if &dos[..2] != b"MZ" {
        bail!("EFI binary {} lacks MZ header", path.display());
    }
    let offset = u32::from_le_bytes(dos[0x3c..0x40].try_into()?) as u64;
    if !(64..=1024 * 1024).contains(&offset) {
        bail!("Invalid EFI PE header offset in {}", path.display());
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut pe = [0u8; 6];
    file.read_exact(&mut pe)?;
    if &pe[..4] != b"PE\0\0" {
        bail!("EFI binary {} lacks PE signature", path.display());
    }
    let actual = u16::from_le_bytes([pe[4], pe[5]]);
    if actual != machine {
        bail!("EFI binary {} targets machine {actual:#06x}, not {machine:#06x}", path.display());
    }
    Ok(())
}

/// Both supported bootloaders use --no-variables / --removable and must
/// therefore provide the architecture-specific UEFI removable-media path.
pub fn verify_efi_binary(root: &Path, arch: &str, systemd: bool) -> Result<()> {
    let (fallback, installed) = match arch {
        "amd64" => ("BOOTX64.EFI", "systemd-bootx64.efi"),
        "arm64" => ("BOOTAA64.EFI", "systemd-bootaa64.efi"),
        other => bail!("Unknown EFI architecture {other}"),
    };
    check_pe(&root.join("boot/EFI/BOOT").join(fallback), arch)?;
    if systemd {
        check_pe(&root.join("boot/EFI/systemd").join(installed), arch)?;
    }
    Ok(())
}

fn kernel_options(options: &str, filesystem: Filesystem, actual_uuid: Option<&str>) -> Result<()> {
    let root_params: Vec<_> = options.split_whitespace()
        .filter_map(|v| v.strip_prefix("root=")).collect();
    if root_params.len() != 1 {
        bail!("Boot entry requires exactly one root= argument");
    }
    let target = root_params[0];
    let is_label = target == "LABEL=ROOT";
    let is_uuid = actual_uuid.is_some_and(|uuid| target
        .strip_prefix("UUID=").is_some_and(|value| value.eq_ignore_ascii_case(uuid)));
    if !is_label && !is_uuid {
        bail!("Boot root {target} does not identify the installed ROOT filesystem");
    }
    let flags: Vec<_> = options.split_whitespace()
        .filter_map(|v| v.strip_prefix("rootflags=")).collect();
    if filesystem == Filesystem::Btrfs {
        if flags != ["subvol=@"] {
            bail!("Btrfs boot entry requires rootflags=subvol=@");
        }
    } else if !flags.is_empty() {
        bail!("Unexpected rootflags for ext4 root");
    }
    Ok(())
}

pub fn verify_systemd_entries(root: &Path, filesystem: Filesystem) -> Result<()> {
    if fs::read_to_string(root.join("etc/kernel/entry-token"))?.trim() != TOKEN {
        bail!("kernel-install entry-token does not match Stagehand loader default");
    }
    let install_conf = fs::read_to_string(root.join("etc/kernel/install.conf"))?;
    if !install_conf.lines().any(|line| line.trim() == "layout=bls") {
        bail!("kernel-install layout is not bls");
    }
    let loader = fs::read_to_string(root.join("boot/loader/loader.conf"))?;
    if !loader.lines().any(|line| line.trim() == "default gentoo-*") {
        bail!("systemd-boot default does not select Stagehand's Gentoo entries");
    }
    let expected_cmdline = fs::read_to_string(root.join("etc/kernel/cmdline"))?;
    kernel_options(&expected_cmdline, filesystem, None)?;
    let entries = root.join("boot/loader/entries");
    let mut found = 0usize;
    for entry in fs::read_dir(&entries)
        .with_context(|| format!("Boot Loader Spec entries missing: {}", entries.display()))?
    {
        let path = entry?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if !name.starts_with("gentoo-") || !name.ends_with(".conf") {
            continue;
        }
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            bail!("Boot Loader Spec entry {} is not a regular file", path.display());
        }
        found += 1;
        let content = fs::read_to_string(&path)?;
        let mut linux = Vec::new();
        let mut initrd = Vec::new();
        let mut options = Vec::new();
        for line in content.lines().map(str::trim) {
            if line.starts_with('#') || line.is_empty() { continue; }
            let mut fields = line.split_whitespace();
            match fields.next() {
                Some("linux") => linux.extend(fields),
                Some("initrd") => initrd.extend(fields),
                Some("options") => options.push(fields.collect::<Vec<_>>().join(" ")),
                _ => {}
            }
        }
        if linux.len() != 1 || initrd.is_empty() || options.len() != 1 {
            bail!("Boot entry {name} must reference exactly one kernel, an initramfs and one options line");
        }
        on_esp(root, linux[0])?;
        for path in initrd { on_esp(root, path)?; }
        if options[0].split_whitespace().collect::<Vec<_>>()
            != expected_cmdline.split_whitespace().collect::<Vec<_>>()
        {
            bail!("Boot entry {name} kernel parameters differ from /etc/kernel/cmdline");
        }
        kernel_options(&options[0], filesystem, None)?;
    }
    if found == 0 {
        bail!("No gentoo-* boot entries match systemd-boot's default setting");
    }
    Ok(())
}

/// Read the default GRUB menuentry and verify that it launches the installed
/// root with an existing kernel+initramfs (not just any Linux image).
pub fn verify_grub_entries(
    root: &Path,
    filesystem: Filesystem,
    installed_root_uuid: &str,
) -> Result<()> {
    let config = fs::read_to_string(root.join("boot/grub/grub.cfg"))?;
    let mut started = false;
    let mut linux = None;
    let mut initrds: Vec<String> = Vec::new();
    for line in config.lines() {
        let line = line.trim_start();
        if line.starts_with("menuentry ") {
            if started { break; }
            started = true;
        } else if started {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("linux" | "linuxefi") if linux.is_none() => {
                    linux = Some((words.next().unwrap_or("").to_string(),
                        words.collect::<Vec<_>>().join(" ")));
                }
                Some("initrd" | "initrdefi") => initrds.extend(words.map(str::to_string)),
                _ => {}
            }
            if line == "}" { break; }
        }
    }
    let (kernel, opts) = linux.context("GRUB default menuentry has no Linux kernel line")?;
    if initrds.is_empty() {
        bail!("GRUB default menuentry has no initramfs line");
    }
    on_esp(root, &kernel)?;
    // The default GRUB menuentry must actually refer to the initramfs of
    // its selected kernel, not merely any stale image on the ESP.
    let kernel_name = Path::new(&kernel).file_name()
        .and_then(|name| name.to_str()).context("GRUB kernel has an invalid filename")?;
    let kernel_version = kernel_name.strip_prefix("vmlinuz-")
        .or_else(|| kernel_name.strip_prefix("kernel-"))
        .context("Unrecognized installed GRUB kernel filename")?;
    let mut matching_initramfs = false;
    for initrd in initrds {
        on_esp(root, &initrd)?;
        let initrd_name = Path::new(&initrd).file_name().and_then(|name| name.to_str())
            .context("Invalid GRUB initramfs filename")?;
        if initrd_name.strip_prefix("initramfs-")
            .and_then(|name| name.strip_suffix(".img")) == Some(kernel_version)
        {
            matching_initramfs = true;
        }
    }
    if !matching_initramfs {
        bail!("GRUB default kernel {kernel_name} has no matching initramfs version");
    }
    kernel_options(&opts, filesystem, Some(installed_root_uuid))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn root() -> std::path::PathBuf {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("stagehand-efi-{}-{now}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn write_file(root: &Path, name: &str, bytes: &[u8]) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn efi(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 8192];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(0x80u32).to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        bytes
    }
    #[test]
    fn checks_removable_efi_machine_and_signature() {
        let root = root();
        let file = "boot/EFI/BOOT/BOOTX64.EFI";
        write_file(&root, file, &efi(0x8664));
        write_file(&root, "boot/EFI/systemd/systemd-bootx64.efi", &efi(0x8664));
        assert!(verify_efi_binary(&root, "amd64", true).is_ok());
        assert!(verify_efi_binary(&root, "arm64", false).is_err());
        write_file(&root, file, &efi(0xaa64));
        assert!(verify_efi_binary(&root, "amd64", true).is_err());
        write_file(&root, file, &vec![1; 8192]);
        assert!(verify_efi_binary(&root, "amd64", false).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn systemd_entry_must_match_kernel_initrd_cmdline_and_default() {
        let root = root();
        write_file(&root, "etc/kernel/entry-token", b"gentoo\n");
        write_file(&root, "etc/kernel/install.conf", b"layout=bls\n");
        write_file(&root, "etc/kernel/cmdline",
            b"console=tty0 root=LABEL=ROOT rootfstype=btrfs rootflags=subvol=@ rw\n");
        write_file(&root, "boot/loader/loader.conf", b"default gentoo-*\ntimeout 3\n");
        write_file(&root, "boot/loader/entries/gentoo-6.12.conf",
            b"title Gentoo\nlinux /gentoo/6.12/linux\ninitrd /gentoo/6.12/initrd\noptions console=tty0 root=LABEL=ROOT rootfstype=btrfs rootflags=subvol=@ rw\n");
        write_file(&root, "boot/gentoo/6.12/linux", &vec![1; 8192]);
        write_file(&root, "boot/gentoo/6.12/initrd", &vec![1; 8192]);
        assert!(verify_systemd_entries(&root, Filesystem::Btrfs).is_ok());
        write_file(&root, "boot/loader/entries/gentoo-6.12.conf",
            b"linux /gentoo/6.12/linux\ninitrd /gentoo/6.12/initrd\noptions root=LABEL=LIVE rw\n");
        assert!(verify_systemd_entries(&root, Filesystem::Btrfs).is_err());
        write_file(&root, "boot/loader/entries/gentoo-6.12.conf",
            b"linux /gentoo/6.12/linux\ninitrd /../etc/shadow\noptions console=tty0 root=LABEL=ROOT rootfstype=btrfs rootflags=subvol=@ rw\n");
        assert!(verify_systemd_entries(&root, Filesystem::Btrfs).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn grub_checks_default_entry_kernel_initrd_and_real_uuid() {
        let root = root();
        let config = "menuentry 'Gentoo' {\n linux /vmlinuz-6 root=UUID=abc-123 rootflags=subvol=@ rw\n initrd /initramfs-6.img\n}\n";
        write_file(&root, "boot/grub/grub.cfg", config.as_bytes());
        write_file(&root, "boot/vmlinuz-6", &vec![1; 8192]);
        write_file(&root, "boot/initramfs-6.img", &vec![1; 8192]);
        assert!(verify_grub_entries(&root, Filesystem::Btrfs, "abc-123").is_ok());
        assert!(verify_grub_entries(&root, Filesystem::Btrfs, "other").is_err());
        write_file(&root, "boot/grub/grub.cfg",
            b"menuentry 'Gentoo' {\n linux /vmlinuz-6 root=UUID=abc-123 rootflags=subvol=@ rw\n initrd /initramfs-7.img\n}\n");
        write_file(&root, "boot/initramfs-7.img", &vec![1; 8192]);
        assert!(verify_grub_entries(&root, Filesystem::Btrfs, "abc-123").is_err());
        write_file(&root, "boot/grub/grub.cfg",
            b"menuentry 'Gentoo' {\n linux /vmlinuz-6 root=UUID=abc-123 rw\n initrd /initramfs-6.img\n}\n");
        assert!(verify_grub_entries(&root, Filesystem::Btrfs, "abc-123").is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rejects_ambiguous_roots_and_wrong_filesystem_flags() {
        assert!(kernel_options("root=LABEL=ROOT root=LABEL=LIVE rw", Filesystem::Ext4, None).is_err());
        assert!(kernel_options("root=LABEL=ROOT rootflags=subvol=@", Filesystem::Ext4, None).is_err());
        assert!(kernel_options("root=LABEL=ROOT rootflags=subvol=@", Filesystem::Btrfs, None).is_ok());
    }
}

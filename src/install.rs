use crate::{config::Config, disks};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::symlink, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::Sender,
    thread,
};

pub const LOG: &str = "/tmp/stagehand-install.log";
pub const STEP_COUNT: usize = 12;
const ROOT: &str = "/mnt/stagehand";
const MIRROR: &str = "https://distfiles.gentoo.org";

pub enum Event {
    Step(usize, String),
    Complete(Result<(), String>),
}

pub fn start(config: Config, tx: Sender<Event>) {
    thread::spawn(move || {
        let result = install(&config, &tx).map_err(|e| format!("{e:#}"));
        let _ = tx.send(Event::Complete(result));
    });
}
fn progress(tx: &Sender<Event>, n: usize, title: &str) {
    let _ = tx.send(Event::Step(n, title.into()));
}
fn install(c: &Config, tx: &Sender<Event>) -> Result<()> {
    progress(tx, 1, "Preflight checks");
    c.validate().map_err(anyhow::Error::msg)?;
    preflight(c)?;
    File::create(LOG)?;

    progress(tx, 2, "Fetch and verify Gentoo stage3");
    let stage3 = fetch_stage3(arch()?)?;

    // This is the destructive boundary: all earlier steps are read-only.
    progress(tx, 3, "Partition selected disk");
    let selected = c.disk.as_ref().context("No disk selected")?;
    disks::verify_target(selected)?;
    let disk = selected.path.as_str();
    exec(
        "parted",
        &[
            "-s", disk, "mklabel", "gpt", "mkpart", "ESP", "fat32", "1MiB", "513MiB", "set", "1",
            "esp", "on", "mkpart", "ROOT", "btrfs", "513MiB", "100%",
        ],
    )?;
    exec("udevadm", &["settle"])?;
    let esp = disks::partition_path(disk, 1);
    let root = disks::partition_path(disk, 2);

    progress(tx, 4, "Format and mount Btrfs subvolumes");
    exec("mkfs.vfat", &["-F", "32", "-n", "EFI", &esp])?;
    exec("mkfs.btrfs", &["-f", "-L", "ROOT", &root])?;
    fs::create_dir_all(ROOT)?;
    let mut guard = MountGuard(false);
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
    fs::create_dir_all(format!("{ROOT}/boot"))?;
    fs::create_dir_all(format!("{ROOT}/home"))?;
    exec("mount", &[&esp, &format!("{ROOT}/boot")])?;
    exec(
        "mount",
        &[
            "-o",
            "noatime,compress=zstd,space_cache=v2,subvol=@home",
            &root,
            &format!("{ROOT}/home"),
        ],
    )?;

    progress(tx, 5, "Extract systemd stage3");
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

    progress(tx, 6, "Configure filesystem, locale and network");
    configure(c)?;

    progress(tx, 7, "Prepare Gentoo chroot");
    for part in ["proc", "sys", "dev", "run"] {
        fs::create_dir_all(format!("{ROOT}/{part}"))?;
    }
    exec("mount", &["-t", "proc", "proc", &format!("{ROOT}/proc")])?;
    exec("mount", &["--rbind", "/sys", &format!("{ROOT}/sys")])?;
    exec("mount", &["--make-rslave", &format!("{ROOT}/sys")])?;
    exec("mount", &["--rbind", "/dev", &format!("{ROOT}/dev")])?;
    exec("mount", &["--make-rslave", &format!("{ROOT}/dev")])?;
    exec("mount", &["--bind", "/run", &format!("{ROOT}/run")])?;

    progress(tx, 8, "Initialize Portage tree");
    inside(&["/usr/bin/emerge-webrsync"])?;

    progress(tx, 9, "Install kernel, boot and essential packages");
    let mut packages = vec![
        "/usr/bin/emerge",
        "--noreplace",
        "sys-kernel/installkernel",
        "sys-kernel/gentoo-kernel-bin",
        "sys-kernel/linux-firmware",
        "sys-fs/btrfs-progs",
        "app-admin/sudo",
    ];
    if c.ssh_enabled {
        packages.push("net-misc/openssh");
    }
    inside(&packages)?;

    progress(tx, 10, "Create accounts and credentials");
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

    progress(tx, 11, "Enable services and configure systemd-boot");
    let mut services = vec![
        "/usr/bin/systemctl",
        "enable",
        "systemd-networkd",
        "systemd-resolved",
        "systemd-timesyncd",
    ];
    if c.ssh_enabled {
        services.push("sshd");
    }
    inside(&services)?;
    let resolver = Path::new(ROOT).join("etc/resolv.conf");
    fs::remove_file(&resolver)?;
    symlink("../run/systemd/resolve/stub-resolv.conf", &resolver)?;
    inside(&[
        "/usr/bin/bootctl",
        "--esp-path=/boot",
        "install",
        "--no-variables",
    ])?;
    write(
        "boot/loader/loader.conf",
        "default gentoo-*\ntimeout 3\neditor no\n",
    )?;
    let entries = Path::new(ROOT).join("boot/loader/entries");
    let ukis = Path::new(ROOT).join("boot/EFI/Linux");
    let has_entries = entries.is_dir()
        && fs::read_dir(entries)?.any(|e| {
            e.ok()
                .is_some_and(|f| f.path().extension().is_some_and(|s| s == "conf"))
        });
    let has_ukis = ukis.is_dir()
        && fs::read_dir(ukis)?.any(|e| {
            e.ok()
                .is_some_and(|f| f.path().extension().is_some_and(|s| s == "efi"))
        });
    if !has_entries && !has_ukis {
        bail!("No systemd-boot entry or UKI found. Inspect the log before rebooting.");
    }

    progress(tx, 12, "Sync disks and safely unmount");
    exec("sync", &[])?;
    exec("umount", &["-R", ROOT])?;
    guard.0 = false;
    Ok(())
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
        if !Command::new("sh")
            .args(["-c", "command -v \"$1\" >/dev/null", "sh", command])
            .status()?
            .success()
        {
            bail!("Missing live-system dependency: {command}");
        }
    }
    disks::verify_target(c.disk.as_ref().context("Select a disk")?)?;
    if Path::new(ROOT).exists() && fs::read_dir(ROOT)?.next().is_some() {
        bail!("{ROOT} is not empty. Resolve old mounts first.");
    }
    Ok(())
}
fn arch() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        other => bail!("Unsupported architecture: {other}"),
    }
}
fn fetch_stage3(a: &str) -> Result<PathBuf> {
    let base = format!("{MIRROR}/releases/{a}/autobuilds/current-stage3-{a}-systemd");
    let index = output(
        "curl",
        &[
            "-fsSL",
            "--retry",
            "3",
            &format!("{base}/latest-stage3-{a}-systemd.txt"),
        ],
    )?;
    let name = parse_name(&index, a)?;
    let checksum = output(
        "curl",
        &["-fsSL", "--retry", "3", &format!("{base}/{name}.sha256")],
    )?;
    let expected = checksum
        .split_whitespace()
        .find(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("Malformed stage3 SHA-256 checksum"))?
        .to_ascii_lowercase();
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
fn parse_name(index: &str, a: &str) -> Result<String> {
    index
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let size = fields.next()?;
            if fields.next().is_some()
                || size.parse::<u64>().is_err()
                || !name.starts_with(&format!("stage3-{a}-systemd-"))
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
fn configure(c: &Config) -> Result<()> {
    write("etc/fstab", "# Stagehand\nLABEL=ROOT / btrfs noatime,compress=zstd,space_cache=v2,subvol=@ 0 0\nLABEL=ROOT /home btrfs noatime,compress=zstd,space_cache=v2,subvol=@home 0 0\nLABEL=EFI /boot vfat defaults,noatime 0 2\n")?;
    write("etc/hostname", &format!("{}\n", c.hostname))?;
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
    write(
        "etc/systemd/network/20-wired.network",
        "[Match]\nName=en* eth*\n\n[Network]\nDHCP=yes\n",
    )?;
    write("etc/portage/package.use/stagehand",
        "sys-apps/systemd boot\nsys-apps/systemd-utils boot kernel-install\nsys-kernel/installkernel systemd systemd-boot dracut\n")?;
    write(
        "etc/portage/package.license/stagehand",
        "sys-kernel/linux-firmware linux-fw-redistributable\n",
    )?;
    write(
        "etc/kernel/cmdline",
        "console=tty0 root=LABEL=ROOT rootfstype=btrfs rootflags=subvol=@ rw\n",
    )?;
    // Gentoo reads /etc/portage/make.conf, not a make.conf.d directory.
    let make = format!("{ROOT}/etc/portage/make.conf");
    let mut f = OpenOptions::new().append(true).open(&make)?;
    f.write_all(b"\n# Stagehand: prefer binary packages\nFEATURES=\"\x24{FEATURES} getbinpkg\"\nEMERGE_DEFAULT_OPTS=\"\x24{EMERGE_DEFAULT_OPTS} --getbinpkg=y --usepkg=y\"\n")?;
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
fn inside(args: &[&str]) -> Result<()> {
    let mut all = vec![ROOT];
    all.extend_from_slice(args);
    exec("chroot", &all)
}
fn inside_secret(args: &[&str], secret: &[u8]) -> Result<()> {
    let mut log = OpenOptions::new().append(true).create(true).open(LOG)?;
    writeln!(log, "> chroot [stdin redacted] {}", args.join(" "))?;
    let mut p = Command::new("chroot")
        .arg(ROOT)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    p.stdin
        .take()
        .context("Could not pipe credentials")?
        .write_all(secret)?;
    if !p.wait()?.success() {
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
            parse_name("stage3-amd64-systemd-20261116.tar.xz 123\n", "amd64").unwrap(),
            "stage3-amd64-systemd-20261116.tar.xz"
        );
        assert!(parse_name("../bad 42", "amd64").is_err());
    }
}

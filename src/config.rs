use crate::disks::Disk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage3 {
    Base,
    Desktop,
}
impl Stage3 {
    pub fn label(self) -> &'static str {
        match self {
            Self::Base => "Systemd base (default)",
            Self::Desktop => "Systemd desktop",
        }
    }
    pub fn archive_prefix(self, architecture: &str) -> String {
        match self {
            Self::Base => format!("stage3-{architecture}-systemd"),
            Self::Desktop => format!("stage3-{architecture}-desktop-systemd"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Networkd,
    NetworkManager,
}
impl Network {
    pub fn label(self) -> &'static str {
        match self {
            Self::Networkd => "systemd-networkd (wired DHCP)",
            Self::NetworkManager => "NetworkManager (Wi-Fi capable)",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildJobs {
    Auto,
    Two,
    Four,
}
impl BuildJobs {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (Portage default)",
            Self::Two => "2 parallel build jobs",
            Self::Four => "4 parallel build jobs",
        }
    }
    pub fn makeopts(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Two => Some("-j2"),
            Self::Four => Some("-j4"),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Editor {
    None,
    Nano,
    Vim,
}
impl Editor {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Nano => "nano (default)",
            Self::Vim => "vim",
        }
    }
    pub fn package(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Nano => Some("app-editors/nano"),
            Self::Vim => Some("app-editors/vim"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filesystem {
    Btrfs,
    Ext4,
}
impl Filesystem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Btrfs => "Btrfs (zstd, @ and @home)",
            Self::Ext4 => "ext4 (single root)",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swap {
    None,
    FourGiB,
}
impl Swap {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No swap (default)",
            Self::FourGiB => "4 GiB swap partition",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    English,
    German,
    Utf8,
}
impl Locale {
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "en_US.UTF-8",
            Self::German => "de_DE.UTF-8",
            Self::Utf8 => "C.UTF-8",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    Binary,
    Source,
}
impl Kernel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Binary => "gentoo-kernel-bin",
            Self::Source => "gentoo-kernel",
        }
    }
    pub fn package(self) -> &'static str {
        match self {
            Self::Binary => "sys-kernel/gentoo-kernel-bin",
            Self::Source => "sys-kernel/gentoo-kernel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bootloader {
    SystemdBoot,
    Grub,
}
impl Bootloader {
    pub fn label(self) -> &'static str {
        match self {
            Self::SystemdBoot => "systemd-boot",
            Self::Grub => "GRUB (UEFI)",
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub disk: Option<Disk>,
    pub stage3: Stage3,
    pub network: Network,
    pub build_jobs: BuildJobs,
    pub editor: Editor,
    pub cronie: bool,
    pub filesystem: Filesystem,
    pub swap: Swap,
    pub locale: Locale,
    pub binary_packages: bool,
    pub kernel: Kernel,
    pub bootloader: Bootloader,
    pub hostname: String,
    pub timezone: String,
    pub keymap: String,
    pub username: String,
    pub user_password: String,
    pub root_password: String,
    pub ssh_enabled: bool,
    pub ssh_public_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            disk: None,
            stage3: Stage3::Base,
            network: Network::Networkd,
            build_jobs: BuildJobs::Auto,
            editor: Editor::Nano,
            cronie: false,
            filesystem: Filesystem::Btrfs,
            swap: Swap::None,
            locale: Locale::English,
            binary_packages: true,
            kernel: Kernel::Binary,
            bootloader: Bootloader::SystemdBoot,
            hostname: "gentoo".into(),
            timezone: "Europe/Berlin".into(),
            keymap: "de".into(),
            username: String::new(),
            user_password: String::new(),
            root_password: String::new(),
            ssh_enabled: false,
            ssh_public_key: String::new(),
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        let disk = self.disk.as_ref().ok_or("Select an installation disk.")?;
        if self.swap == Swap::FourGiB && disk.bytes < 16 * 1_073_741_824 {
            return Err("A 4 GiB swap partition requires a disk of at least 16 GiB.".into());
        }
        if disk.block_reason.is_some() {
            return Err("The selected disk is mounted, removable, read-only, or too small.".into());
        }
        if !valid_hostname(&self.hostname) {
            return Err("Hostname must be 1-63 ASCII letters, digits or hyphens, and cannot start/end with '-'.".into());
        }
        if !valid_timezone(&self.timezone) {
            return Err("Enter a valid relative timezone, e.g. Europe/Berlin.".into());
        }
        if self.keymap.is_empty()
            || self.keymap.len() > 32
            || !self
                .keymap
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err("Keymap must contain only lowercase letters, digits, '-' or '_'.".into());
        }
        if !valid_username(&self.username) {
            return Err("Username must start with a lowercase letter and contain only lowercase letters, digits, '_' or '-'.".into());
        }
        if self.user_password.chars().count() < 8 || self.root_password.chars().count() < 8 {
            return Err("User and root passwords must each contain at least 8 characters.".into());
        }
        if self.user_password.chars().any(|c| c == '\n' || c == '\r')
            || self.root_password.chars().any(|c| c == '\n' || c == '\r')
        {
            return Err("Passwords cannot contain newlines.".into());
        }
        if self.ssh_enabled && !valid_ssh_key(&self.ssh_public_key) {
            return Err("SSH requires a public key (ssh-ed25519, ssh-rsa or ecdsa-sha2-*). Password login is disabled.".into());
        }
        Ok(())
    }
}

fn valid_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 63
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && host
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && host
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_username(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user != "root"
        && user.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && user
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_timezone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 128
        && zone
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && zone
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
}

fn valid_ssh_key(key: &str) -> bool {
    let parts: Vec<_> = key.split_whitespace().collect();
    parts.len() >= 2
        && (matches!(parts[0], "ssh-ed25519" | "ssh-rsa") || parts[0].starts_with("ecdsa-sha2-"))
        && parts[1].len() >= 32
        && parts[1]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_requires_disk_and_credentials() {
        assert!(Config::default().validate().is_err());
        assert_eq!(Config::default().stage3, Stage3::Base);
        assert_eq!(Config::default().network, Network::Networkd);
        assert_eq!(Config::default().build_jobs, BuildJobs::Auto);
        assert_eq!(Config::default().editor, Editor::Nano);
        assert!(!Config::default().cronie);
        assert_eq!(Config::default().filesystem, Filesystem::Btrfs);
        assert_eq!(Config::default().swap, Swap::None);
        assert_eq!(Config::default().locale, Locale::English);
        assert!(Config::default().binary_packages);
        assert_eq!(Config::default().kernel, Kernel::Binary);
        assert_eq!(Config::default().bootloader, Bootloader::SystemdBoot);
    }

    #[test]
    fn reject_path_traversal_and_shell_names() {
        assert!(!valid_timezone("../etc/passwd"));
        assert!(!valid_timezone("/etc/passwd"));
        assert!(!valid_hostname("oops;reboot"));
        assert!(!valid_username("root"));
        assert!(!valid_username("UPPER"));
    }

    #[test]
    fn swap_requires_sufficient_disk_size() {
        let c = Config {
            disk: Some(crate::disks::Disk {
                path: "/dev/vda".to_owned(),
                bytes: 8 * 1_073_741_824,
                model: "Test".to_owned(),
                contents: crate::disks::DiskContents::NoSignatures,
                block_reason: None,
            }),
            swap: Swap::FourGiB,
            ..Config::default()
        };
        assert!(c.validate().unwrap_err().contains("16 GiB"));
    }

    #[test]
    fn labels_match_real_installation_choices() {
        assert_eq!(Filesystem::Btrfs.label(), "Btrfs (zstd, @ and @home)");
        assert_eq!(Filesystem::Ext4.label(), "ext4 (single root)");
        assert_eq!(Locale::German.label(), "de_DE.UTF-8");
    }

    #[test]
    fn accept_common_settings() {
        assert_eq!(Stage3::Base.archive_prefix("amd64"), "stage3-amd64-systemd");
        assert_eq!(
            Stage3::Desktop.archive_prefix("arm64"),
            "stage3-arm64-desktop-systemd"
        );
        assert_eq!(BuildJobs::Auto.makeopts(), None);
        assert_eq!(BuildJobs::Four.makeopts(), Some("-j4"));
        assert_eq!(Editor::Nano.package(), Some("app-editors/nano"));
        assert!(valid_hostname("gentoo-test"));
        assert!(valid_timezone("Europe/Berlin"));
        assert!(valid_username("dan"));
    }
}

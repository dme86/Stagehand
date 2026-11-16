use crate::disks::Disk;

#[derive(Clone)]
pub struct Config {
    pub disk: Option<Disk>,
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
        if self.disk.is_none() {
            return Err("Select an installation disk.".into());
        }
        if !valid_hostname(&self.hostname) {
            return Err("Hostname must be 1-63 ASCII letters, digits or hyphens, and cannot start/end with '-'.".into());
        }
        if !valid_timezone(&self.timezone) {
            return Err("Enter a valid relative timezone, e.g. Europe/Berlin.".into());
        }
        if self.keymap.is_empty()
            || self.keymap.len() > 32
            || !self.keymap.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err("Keymap must contain only lowercase letters, digits, '-' or '_'.".into());
        }
        if !valid_username(&self.username) {
            return Err("Username must start with a lowercase letter and contain only lowercase letters, digits, '_' or '-'.".into());
        }
        if self.user_password.chars().count() < 8 || self.root_password.chars().count() < 8 {
            return Err("User and root passwords must each contain at least 8 characters.".into());
        }
        if self.user_password.contains(['\n', '\r']) || self.root_password.contains(['\n', '\r']) {
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
        && host.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && host.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
}

fn valid_username(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user != "root"
        && user.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && user.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_timezone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 128
        && zone.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && zone.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
}

fn valid_ssh_key(key: &str) -> bool {
    let parts: Vec<_> = key.split_whitespace().collect();
    parts.len() >= 2
        && (matches!(parts[0], "ssh-ed25519" | "ssh-rsa") || parts[0].starts_with("ecdsa-sha2-"))
        && parts[1].len() >= 32
        && parts[1].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_requires_disk_and_credentials() {
        assert!(Config::default().validate().is_err());
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
    fn accept_common_settings() {
        assert!(valid_hostname("gentoo-test"));
        assert!(valid_timezone("Europe/Berlin"));
        assert!(valid_username("dan"));
    }
}

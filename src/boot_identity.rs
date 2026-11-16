//! Each installation gets a unique stable systemd machine-id and, for
//! systemd-boot, a separate public BLS entry token. Neither comes from the
//! live ISO, and future kernel-install runs reuse the token on disk.
use crate::config::Bootloader;
use anyhow::{bail, Context, Result};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

const PREFIX: &str = "gentoo-";
const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 15) as usize] as char);
    }
    out
}

fn new_identity_values<R: Read>(random: &mut R) -> Result<(String, String)> {
    let mut machine = [0u8; 16];
    random
        .read_exact(&mut machine)
        .context("Read cryptographic machine-id entropy")?;
    // Set the conventional UUID version/variant bits; the file format stays
    // the 32 lowercase hexadecimal characters required by systemd.
    machine[6] = (machine[6] & 0x0f) | 0x40;
    machine[8] = (machine[8] & 0x3f) | 0x80;
    let machine_id = hex(&machine);

    // Independent randomness: do not publish/derive the sensitive
    // machine-id in filenames on an unencrypted EFI system partition.
    let mut token_bytes = [0u8; 12];
    random
        .read_exact(&mut token_bytes)
        .context("Read unique boot entry token entropy")?;
    let token = format!("{PREFIX}{}", hex(&token_bytes));
    Ok((machine_id, token))
}

fn valid_machine_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && value.bytes().any(|b| b != b'0')
}
fn valid_entry_token(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(|suffix| {
        suffix.len() == 24
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn write_new(root: &Path, path: &str, value: &str) -> Result<()> {
    let target = root.join(path);
    fs::create_dir_all(target.parent().context("Missing parent directory")?)?;
    if target.is_symlink() {
        bail!(
            "Refusing to write boot identity through symlink {}",
            target.display()
        );
    }
    fs::write(&target, format!("{value}\n")).with_context(|| format!("Write {}", target.display()))
}

/// Called *after* extracting stage3, *before* any Portage/kernel-install
/// invocation. The resulting machine-id remains stable on first boot.
pub fn prepare(root: &Path, loader: Bootloader) -> Result<()> {
    let mut random = File::open("/dev/urandom").context("Open OS random source")?;
    let (machine_id, token) = new_identity_values(&mut random)?;
    write_new(root, "etc/machine-id", &machine_id)?;
    if loader == Bootloader::SystemdBoot {
        write_new(root, "etc/kernel/entry-token", &token)?;
        let install_conf = root.join("etc/kernel/install.conf");
        fs::create_dir_all(install_conf.parent().context("Missing /etc/kernel")?)?;
        if install_conf.is_symlink() {
            bail!("Refusing to overwrite symlink {}", install_conf.display());
        }
        fs::write(install_conf, "layout=bls\n")?;
    }
    Ok(())
}

pub fn machine_id(root: &Path) -> Result<String> {
    let value =
        fs::read_to_string(root.join("etc/machine-id")).context("Missing installed machine-id")?;
    let value = value.trim();
    if !valid_machine_id(value) {
        bail!("Installed system does not have a valid stable machine-id");
    }
    Ok(value.to_owned())
}
pub fn entry_token(root: &Path) -> Result<String> {
    // Reading the machine-id also proves it wasn't left empty or replaced
    // with the temporary identity of the installation environment.
    machine_id(root)?;
    let value = fs::read_to_string(root.join("etc/kernel/entry-token"))
        .context("Missing kernel-install entry-token")?;
    let value = value.trim();
    if !valid_entry_token(value) {
        bail!("Unexpected or invalid kernel-install entry-token");
    }
    Ok(value.to_owned())
}
pub fn loader_default(token: &str) -> Result<String> {
    if !valid_entry_token(token) {
        bail!("Cannot derive loader default from invalid entry token");
    }
    Ok(format!("{token}-*"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_ids_are_distinct_and_valid() {
        let input = (0u8..28).collect::<Vec<_>>();
        let (machine, token) = new_identity_values(&mut input.as_slice()).unwrap();
        assert!(valid_machine_id(&machine));
        assert!(valid_entry_token(&token));
        assert_eq!(machine.len(), 32);
        assert_eq!(token.len(), PREFIX.len() + 24);
        assert!(!token.contains(&machine));
        assert_eq!(loader_default(&token).unwrap(), format!("{token}-*"));
    }
    #[test]
    fn malformed_identity_or_short_entropy_is_rejected() {
        assert!(new_identity_values(&mut [1u8; 3].as_slice()).is_err());
        assert!(!valid_machine_id(""));
        assert!(!valid_machine_id(&"0".repeat(32)));
        assert!(!valid_machine_id("0123456789abcdef0123456789ABCDEF"));
        assert!(!valid_entry_token("gentoo"));
        assert!(!valid_entry_token("gentoo-foobar"));
        assert!(loader_default("gentoo-*").is_err());
    }
    #[test]
    fn preparation_sets_a_unique_persistent_machine_id_before_kernel_install() {
        let root = std::env::temp_dir().join(format!(
            "stagehand-identity-prep-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let result = (|| -> Result<()> {
            fs::create_dir_all(root.join("etc/kernel"))?;
            // Typical Stage3 has an empty machine-id, never the live ISO's.
            fs::write(root.join("etc/machine-id"), "")?;
            prepare(&root, Bootloader::SystemdBoot)?;
            let machine = machine_id(&root)?;
            let token = entry_token(&root)?;
            assert_ne!(machine, "0".repeat(32));
            assert!(!token.contains(&machine));
            assert_eq!(
                fs::read_to_string(root.join("etc/kernel/install.conf"))?,
                "layout=bls\n"
            );
            // Simulate a later read on a subsequent boot/kernel update:
            assert_eq!(machine_id(&root)?, machine);
            assert_eq!(entry_token(&root)?, token);
            Ok(())
        })();
        let _ = fs::remove_dir_all(root);
        result.unwrap();
    }
    #[test]
    fn independent_identity_roundtrips() {
        let root = std::env::temp_dir().join(format!(
            "stagehand-identities-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let result = (|| -> Result<()> {
            fs::create_dir_all(root.join("etc/kernel"))?;
            fs::write(root.join("etc/machine-id"), format!("{}\n", "a".repeat(32)))?;
            fs::write(
                root.join("etc/kernel/entry-token"),
                format!("gentoo-{}\n", "b".repeat(24)),
            )?;
            assert_eq!(machine_id(&root)?, "a".repeat(32));
            assert_eq!(entry_token(&root)?, format!("gentoo-{}", "b".repeat(24)));
            fs::write(root.join("etc/machine-id"), "")?;
            assert!(entry_token(&root).is_err());
            Ok(())
        })();
        let _ = fs::remove_dir_all(&root);
        result.unwrap();
    }
}

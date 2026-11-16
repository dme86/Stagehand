use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Command;

#[derive(Clone)]
pub struct Disk {
    pub path: String,
    pub bytes: u64,
    pub model: String,
}

impl Disk {
    pub fn size_label(&self) -> String {
        format!("{:.1} GiB", self.bytes as f64 / 1_073_741_824.0)
    }
}

pub fn discover() -> Result<Vec<Disk>> {
    let output = Command::new("lsblk")
        .args([
            "--json",
            "--bytes",
            "--output",
            "PATH,TYPE,SIZE,MODEL,RO,RM,MOUNTPOINTS",
        ])
        .output()
        .context("Run lsblk (required to discover target disks)")?;
    if !output.status.success() {
        bail!("lsblk failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let json: Value = serde_json::from_slice(&output.stdout).context("Parse lsblk JSON")?;
    let mut disks = Vec::new();
    for node in json["blockdevices"]
        .as_array()
        .context("Missing blockdevices")?
    {
        if node["type"].as_str() != Some("disk")
            || flag(&node["ro"])
            || flag(&node["rm"])
            || is_mounted(node)
        {
            continue;
        }
        let Some(path) = node["path"].as_str() else {
            continue;
        };
        if !path.starts_with("/dev/") {
            continue;
        }
        let bytes = node["size"]
            .as_u64()
            .or_else(|| node["size"].as_str().and_then(|v| v.parse().ok()))
            .unwrap_or(0);
        if bytes < 8 * 1_073_741_824 {
            continue;
        }
        disks.push(Disk {
            path: path.to_string(),
            bytes,
            model: node["model"]
                .as_str()
                .unwrap_or("Unknown disk")
                .trim()
                .to_string(),
        });
    }
    disks.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(disks)
}

fn flag(value: &Value) -> bool {
    value.as_bool() == Some(true) || value.as_u64() == Some(1) || value.as_str() == Some("1")
}

fn is_mounted(node: &Value) -> bool {
    if node["mountpoints"].as_array().is_some_and(|mounts| {
        mounts
            .iter()
            .any(|point| point.as_str().is_some_and(|s| !s.is_empty()))
    }) {
        return true;
    }
    node["children"]
        .as_array()
        .is_some_and(|children| children.iter().any(is_mounted))
}

/// Recheck the chosen device immediately before any destructive operation.
pub fn verify_target(selected: &Disk) -> Result<()> {
    let present = discover()?;
    if !present
        .iter()
        .any(|disk| disk.path == selected.path && disk.bytes == selected.bytes)
    {
        bail!(
            "Disk {} is missing, mounted, read-only, removable, or has changed size. Aborting.",
            selected.path
        );
    }
    let output = Command::new("swapon")
        .args(["--noheadings", "--raw", "--output", "NAME"])
        .output()
        .context("Check for active swap")?;
    if !output.status.success() {
        bail!("Cannot verify active swap devices; refusing to erase a disk.");
    }
    let prefix = if selected.path.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{}p", selected.path)
    } else {
        selected.path.clone()
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line == selected.path || line.starts_with(&prefix) {
            bail!("Selected disk contains active swap: {line}");
        }
    }
    Ok(())
}

pub fn partition_path(disk: &str, number: usize) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{disk}p{number}")
    } else {
        format!("{disk}{number}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partition_names() {
        assert_eq!(partition_path("/dev/vda", 1), "/dev/vda1");
        assert_eq!(partition_path("/dev/nvme0n1", 2), "/dev/nvme0n1p2");
        assert_eq!(partition_path("/dev/mmcblk0", 1), "/dev/mmcblk0p1");
    }
}

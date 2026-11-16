use serde::{Deserialize, Serialize};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiskContents {
    NoSignatures,
    ExistingLayout,
    Unknown,
}
impl DiskContents {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoSignatures => "No signatures detected",
            Self::ExistingLayout => "Existing data / partitions",
            Self::Unknown => "Contents unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Disk {
    pub path: String,
    pub bytes: u64,
    pub model: String,
    pub contents: DiskContents,
    pub block_reason: Option<String>,
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
            "PATH,TYPE,SIZE,MODEL,RO,RM,MOUNTPOINTS,PTTYPE,FSTYPE",
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
        if node["type"].as_str() != Some("disk") {
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
        let block_reason = if is_mounted(node) {
            Some("Mounted filesystem".to_string())
        } else if flag(&node["ro"]) {
            Some("Read-only".to_string())
        } else if flag(&node["rm"]) {
            Some("Removable".to_string())
        } else if bytes < 8 * 1_073_741_824 {
            Some("Too small (< 8 GiB)".to_string())
        } else {
            None
        };
        let contents = assess_contents(node, disk_signatures(path));
        disks.push(Disk {
            path: path.to_string(),
            bytes,
            contents,
            block_reason,
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

fn assess_contents(node: &Value, signature_found: Option<bool>) -> DiskContents {
    if node["pttype"].as_str().is_some_and(|s| !s.is_empty())
        || node["fstype"].as_str().is_some_and(|s| !s.is_empty())
        || node["children"]
            .as_array()
            .is_some_and(|children| !children.is_empty())
        || signature_found == Some(true)
    {
        DiskContents::ExistingLayout
    } else if signature_found == Some(false) {
        DiskContents::NoSignatures
    } else {
        DiskContents::Unknown
    }
}

// Conservative, non-destructive probe. This detects known filesystem signatures,
// not hidden data in arbitrary sectors.
fn disk_signatures(device: &str) -> Option<bool> {
    let output = Command::new("wipefs")
        .args(["--no-act", "--json", device])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let doc: Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(!doc["signatures"].as_array()?.is_empty())
}

/// Recheck the chosen device immediately before any destructive operation.
pub fn verify_target(selected: &Disk) -> Result<()> {
    let present = discover()?;
    let current = present
        .iter()
        .find(|disk| disk.path == selected.path && disk.bytes == selected.bytes)
        .context("Selected disk disappeared or changed size")?;
    if let Some(reason) = &current.block_reason {
        bail!("Cannot erase disk {}: {reason}", selected.path);
    }
    if current.contents != selected.contents {
        bail!(
            "Disk {} state changed. Refresh and reselect the disk.",
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
    fn classify_existing_and_blank() {
        let blank = serde_json::json!({"pttype":null,"fstype":null});
        assert_eq!(
            assess_contents(&blank, Some(false)),
            DiskContents::NoSignatures
        );
        assert_eq!(assess_contents(&blank, None), DiskContents::Unknown);
        let used = serde_json::json!({"pttype":"gpt","children":[{"type":"part"}]});
        assert_eq!(assess_contents(&used, None), DiskContents::ExistingLayout);
    }

    #[test]
    fn partition_names() {
        assert_eq!(partition_path("/dev/vda", 1), "/dev/vda1");
        assert_eq!(partition_path("/dev/nvme0n1", 2), "/dev/nvme0n1p2");
        assert_eq!(partition_path("/dev/mmcblk0", 1), "/dev/mmcblk0p1");
    }
}

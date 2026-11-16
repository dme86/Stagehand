mod boot_identity;
mod boot_verify;
mod config;
mod disks;
mod install;
mod ui;
mod worker;

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => ui::run(),
        Some("--worker") => worker::run(),
        Some(_) => anyhow::bail!("Usage: stagehand"),
    }
}

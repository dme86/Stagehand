mod config;
mod disks;
mod install;
mod ui;

fn main() -> anyhow::Result<()> {
    ui::run()
}

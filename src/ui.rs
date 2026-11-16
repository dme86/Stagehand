use crate::{
    config::{Bootloader, Config, Kernel},
    disks::{self, Disk},
    install::{self, Event},
};
use anyhow::Result;
use crossterm::{
    event::{self, Event as Input, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

const BG: Color = Color::Rgb(40, 40, 40);
const PANEL: Color = Color::Rgb(60, 56, 54);
const AMBER: Color = Color::Rgb(250, 189, 47);
const AQUA: Color = Color::Rgb(142, 192, 124);
const RED: Color = Color::Rgb(251, 73, 52);
const FG: Color = Color::Rgb(235, 219, 178);
const MUTED: Color = Color::Rgb(168, 153, 132);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Storage,
    System,
    Boot,
    Network,
    Users,
}
impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Storage => "Storage",
            Self::System => "System",
            Self::Boot => "Kernel & boot",
            Self::Network => "Network",
            Self::Users => "Users",
        }
    }
    fn entries(self) -> &'static [&'static str] {
        match self {
            Self::Storage => &["Installation disk"],
            Self::System => &["Hostname", "Timezone", "Console keymap"],
            Self::Boot => &["Kernel", "Bootloader"],
            Self::Network => &["SSH service", "SSH public key"],
            Self::Users => &["Username", "User password", "Root password"],
        }
    }
}
#[derive(Clone, Copy)]
enum Field {
    Hostname,
    Timezone,
    Keymap,
    Username,
    UserPassword,
    RootPassword,
    SshKey,
}
impl Field {
    fn title(self) -> &'static str {
        match self {
            Self::Hostname => "Hostname",
            Self::Timezone => "Timezone",
            Self::Keymap => "Console keymap",
            Self::Username => "Username",
            Self::UserPassword => "User password",
            Self::RootPassword => "Root password",
            Self::SshKey => "SSH public key",
        }
    }
    fn section(self) -> Section {
        match self {
            Self::Hostname | Self::Timezone | Self::Keymap => Section::System,
            Self::Username | Self::UserPassword | Self::RootPassword => Section::Users,
            Self::SshKey => Section::Network,
        }
    }
    fn secret(self) -> bool {
        matches!(self, Self::UserPassword | Self::RootPassword)
    }
}
#[derive(Clone, Copy)]
enum Choice {
    Kernel,
    Bootloader,
}
#[derive(Clone, Copy)]
enum Page {
    Main,
    Section(Section),
    Disks,
    Edit(Field),
    Pick(Choice),
    Confirm,
    Installing,
    Done,
    Failed,
}

struct App {
    config: Config,
    page: Page,
    main_index: usize,
    section_index: usize,
    disk_index: usize,
    choice_index: usize,
    disks: Vec<Disk>,
    buffer: String,
    pending_password: Option<String>,
    error: String,
    step: usize,
    current_step: String,
    install_error: String,
    receiver: Option<Receiver<Event>>,
}
impl App {
    fn new() -> Self {
        Self {
            config: Config::default(),
            page: Page::Main,
            main_index: 0,
            section_index: 0,
            disk_index: 0,
            choice_index: 0,
            disks: Vec::new(),
            buffer: String::new(),
            pending_password: None,
            error: String::new(),
            step: 0,
            current_step: String::new(),
            install_error: String::new(),
            receiver: None,
        }
    }
    fn update(&mut self) {
        if let Some(receiver) = &self.receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    Event::Step(step, description) => {
                        self.step = step;
                        self.current_step = description;
                    }
                    Event::Complete(Ok(())) => self.page = Page::Done,
                    Event::Complete(Err(error)) => {
                        self.install_error = error;
                        self.page = Page::Failed;
                    }
                }
            }
        }
    }
    fn enter_section(&mut self, section: Section) {
        self.section_index = 0;
        self.error.clear();
        self.page = Page::Section(section);
    }
    fn open_disks(&mut self) {
        match disks::discover() {
            Ok(disks) => {
                self.disks = disks;
                self.disk_index = 0;
                self.error.clear();
                self.page = Page::Disks;
            }
            Err(error) => self.error = format!("Could not discover disks: {error:#}"),
        }
    }
    fn edit(&mut self, field: Field) {
        self.buffer = match field {
            Field::Hostname => self.config.hostname.clone(),
            Field::Timezone => self.config.timezone.clone(),
            Field::Keymap => self.config.keymap.clone(),
            Field::Username => self.config.username.clone(),
            Field::UserPassword | Field::RootPassword => String::new(),
            Field::SshKey => self.config.ssh_public_key.clone(),
        };
        self.pending_password = None;
        self.error.clear();
        self.page = Page::Edit(field);
    }
    fn pick(&mut self, choice: Choice) {
        self.choice_index = match choice {
            Choice::Kernel => usize::from(self.config.kernel == Kernel::Source),
            Choice::Bootloader => usize::from(self.config.bootloader == Bootloader::Grub),
        };
        self.page = Page::Pick(choice);
        self.error.clear();
    }
    fn section_activate(&mut self, section: Section) {
        self.error.clear();
        match (section, self.section_index) {
            (Section::Storage, 0) => self.open_disks(),
            (Section::System, 0) => self.edit(Field::Hostname),
            (Section::System, 1) => self.edit(Field::Timezone),
            (Section::System, 2) => self.edit(Field::Keymap),
            (Section::Boot, 0) => self.pick(Choice::Kernel),
            (Section::Boot, 1) => self.pick(Choice::Bootloader),
            (Section::Network, 0) => self.config.ssh_enabled = !self.config.ssh_enabled,
            (Section::Network, 1) => self.edit(Field::SshKey),
            (Section::Users, 0) => self.edit(Field::Username),
            (Section::Users, 1) => self.edit(Field::UserPassword),
            (Section::Users, 2) => self.edit(Field::RootPassword),
            _ => {}
        }
    }
    fn save_edit(&mut self, field: Field) {
        if field.secret() && self.pending_password.is_none() {
            self.pending_password = Some(std::mem::take(&mut self.buffer));
            self.error.clear();
            return;
        }
        if field.secret() && self.pending_password.as_deref() != Some(self.buffer.as_str()) {
            self.buffer.clear();
            self.pending_password = None;
            self.error = "Passwords did not match. Enter the password again.".into();
            return;
        }
        let value = if field.secret() {
            self.pending_password.take().unwrap_or_default()
        } else {
            std::mem::take(&mut self.buffer)
        };
        match field {
            Field::Hostname => self.config.hostname = value,
            Field::Timezone => self.config.timezone = value,
            Field::Keymap => self.config.keymap = value,
            Field::Username => self.config.username = value,
            Field::UserPassword => self.config.user_password = value,
            Field::RootPassword => self.config.root_password = value,
            Field::SshKey => self.config.ssh_public_key = value,
        }
        self.page = Page::Section(field.section());
        self.error.clear();
    }
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }
        match self.page {
            Page::Main => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return true,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.main_index = self.main_index.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.main_index = (self.main_index + 1).min(5)
                }
                KeyCode::Home => self.main_index = 0,
                KeyCode::End => self.main_index = 5,
                KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                    let section = match self.main_index {
                        0 => Some(Section::Storage),
                        1 => Some(Section::System),
                        2 => Some(Section::Boot),
                        3 => Some(Section::Network),
                        4 => Some(Section::Users),
                        _ => None,
                    };
                    if let Some(section) = section {
                        self.enter_section(section);
                    } else {
                        match self.config.validate() {
                            Ok(()) => {
                                self.buffer.clear();
                                self.error.clear();
                                self.page = Page::Confirm;
                            }
                            Err(error) => self.error = error,
                        }
                    }
                }
                _ => {}
            },
            Page::Section(section) => match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('q') => self.page = Page::Main,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.section_index = self.section_index.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.section_index = (self.section_index + 1).min(section.entries().len() - 1)
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                    self.section_activate(section)
                }
                _ => {}
            },
            Page::Disks => match key.code {
                KeyCode::Esc | KeyCode::Left => self.page = Page::Section(Section::Storage),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.disk_index = self.disk_index.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.disk_index = (self.disk_index + 1).min(self.disks.len().saturating_sub(1))
                }
                KeyCode::Char('r') => self.open_disks(),
                KeyCode::Enter => {
                    if let Some(disk) = self.disks.get(self.disk_index) {
                        if let Some(reason) = &disk.block_reason {
                            self.error = format!("Disk unavailable: {reason}");
                        } else {
                            self.config.disk = Some(disk.clone());
                            self.error.clear();
                            self.page = Page::Section(Section::Storage);
                        }
                    }
                }
                _ => {}
            },
            Page::Pick(choice) => match key.code {
                KeyCode::Esc | KeyCode::Left => self.page = Page::Section(Section::Boot),
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
                    self.choice_index = 1 - self.choice_index;
                }
                KeyCode::Enter | KeyCode::Right => {
                    match choice {
                        Choice::Kernel => {
                            self.config.kernel = if self.choice_index == 0 {
                                Kernel::Binary
                            } else {
                                Kernel::Source
                            }
                        }
                        Choice::Bootloader => {
                            self.config.bootloader = if self.choice_index == 0 {
                                Bootloader::SystemdBoot
                            } else {
                                Bootloader::Grub
                            }
                        }
                    }
                    self.page = Page::Section(Section::Boot);
                    self.error.clear();
                }
                _ => {}
            },
            Page::Edit(field) => match key.code {
                KeyCode::Esc => {
                    self.buffer.clear();
                    self.pending_password = None;
                    self.page = Page::Section(field.section());
                }
                KeyCode::Enter => self.save_edit(field),
                KeyCode::Backspace => {
                    self.buffer.pop();
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.buffer.chars().count() < 4096 =>
                {
                    self.buffer.push(ch)
                }
                _ => {}
            },
            Page::Confirm => match key.code {
                KeyCode::Esc => {
                    self.buffer.clear();
                    self.page = Page::Main;
                }
                KeyCode::Backspace => {
                    self.buffer.pop();
                }
                KeyCode::Enter => {
                    let expected = format!(
                        "ERASE {}",
                        self.config
                            .disk
                            .as_ref()
                            .map(|d| d.path.as_str())
                            .unwrap_or("")
                    );
                    if self.buffer == expected {
                        self.page = Page::Installing;
                        self.step = 0;
                        self.current_step = "Starting".into();
                        self.error.clear();
                        let (tx, rx) = mpsc::channel();
                        self.receiver = Some(rx);
                        install::start(self.config.clone(), tx);
                    } else {
                        self.error = format!("Type exactly: {expected}");
                    }
                }
                KeyCode::Char(ch)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && self.buffer.len() < 128 =>
                {
                    self.buffer.push(ch)
                }
                _ => {}
            },
            Page::Installing => {}
            Page::Done | Page::Failed => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') => return true,
                _ => {}
            },
        }
        false
    }
}
pub fn run() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let result = run_loop(&mut terminal);
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
    result
}
fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();
    loop {
        app.update();
        terminal.draw(|frame| render(frame, &app))?;
        if event::poll(Duration::from_millis(160))? {
            if let Input::Key(key) = event::read()? {
                if app.handle_key(key) {
                    return Ok(());
                }
            }
        }
    }
}
fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(AMBER),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(PANEL))
        .style(Style::default().bg(BG).fg(FG))
}
fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);
    let heading = Paragraph::new(Line::from(vec![
        Span::styled(
            "  STAGEHAND  ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Gentoo installer  ·  v0.2.0  ·  ",
            Style::default().fg(MUTED),
        ),
        Span::styled(arch_label(), Style::default().fg(AQUA)),
    ]))
    .block(panel("Welcome"));
    frame.render_widget(heading, vertical[0]);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(39), Constraint::Percentage(61)])
        .split(vertical[1]);
    render_menu(frame, body[0], app);
    render_details(frame, body[1], app);
    let keys = match app.page {
        Page::Main => " ↑↓/j/k Navigate   Enter Open   q Quit",
        Page::Section(_) => " ↑↓/j/k Navigate   Enter Select   Esc Back",
        Page::Disks => " ↑↓ Select   Enter Choose disk   r Refresh   Esc Back",
        Page::Pick(_) => " ↑↓ Choose option   Enter Save   Esc Back",
        Page::Edit(_) => " Type value   Enter Save   Esc Cancel",
        Page::Confirm => " Type exact ERASE confirmation   Enter Install   Esc Back",
        Page::Installing => " Installation running — do not power off",
        Page::Done | Page::Failed => " Enter / Esc Exit   |   Log: /tmp/stagehand-install.log",
    };
    frame.render_widget(
        Paragraph::new(keys)
            .style(Style::default().fg(AQUA))
            .block(panel("Keys")),
        vertical[2],
    );
}
fn arch_label() -> &'static str {
    static LABEL: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    LABEL.get_or_init(|| match install::arch() {
        Ok("amd64") => "AMD64 / x86_64",
        Ok("arm64") => "ARM64 / aarch64",
        _ => "Unsupported live architecture",
    })
}

fn disk_label(app: &App) -> String {
    app.config
        .disk
        .as_ref()
        .map(|d| format!("{} · {}", d.path, d.contents.label()))
        .unwrap_or_else(|| "Not selected".into())
}
fn section_value(app: &App, section: Section, index: usize) -> String {
    match (section, index) {
        (Section::Storage, 0) => disk_label(app),
        (Section::System, 0) => app.config.hostname.clone(),
        (Section::System, 1) => app.config.timezone.clone(),
        (Section::System, 2) => app.config.keymap.clone(),
        (Section::Boot, 0) => app.config.kernel.label().into(),
        (Section::Boot, 1) => app.config.bootloader.label().into(),
        (Section::Network, 0) => if app.config.ssh_enabled {
            "Enabled"
        } else {
            "Disabled"
        }
        .into(),
        (Section::Network, 1) => if app.config.ssh_public_key.is_empty() {
            "Not set"
        } else {
            "Configured"
        }
        .into(),
        (Section::Users, 0) => {
            if app.config.username.is_empty() {
                "Not set".into()
            } else {
                app.config.username.clone()
            }
        }
        (Section::Users, 1) => if app.config.user_password.is_empty() {
            "Not set"
        } else {
            "Configured"
        }
        .into(),
        (Section::Users, 2) => if app.config.root_password.is_empty() {
            "Not set"
        } else {
            "Configured"
        }
        .into(),
        _ => String::new(),
    }
}
fn render_menu(frame: &mut Frame, area: Rect, app: &App) {
    let section = match app.page {
        Page::Section(section) => Some(section),
        Page::Disks => Some(Section::Storage),
        Page::Pick(_) => Some(Section::Boot),
        Page::Edit(field) => Some(field.section()),
        _ => None,
    };
    let (entries, index, title): (Vec<(String, String)>, usize, &'static str) =
        if let Some(section) = section {
            (
                section
                    .entries()
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.to_string(), section_value(app, section, i)))
                    .collect(),
                app.section_index,
                section.title(),
            )
        } else {
            (
                vec![
                    ("Storage".into(), disk_label(app)),
                    (
                        "System".into(),
                        format!(
                            "{}, {}, {}",
                            app.config.hostname, app.config.timezone, app.config.keymap
                        ),
                    ),
                    (
                        "Kernel & boot".into(),
                        format!(
                            "{} · {}",
                            app.config.kernel.label(),
                            app.config.bootloader.label()
                        ),
                    ),
                    (
                        "Network".into(),
                        if app.config.ssh_enabled {
                            "SSH on"
                        } else {
                            "Wired DHCP · SSH off"
                        }
                        .into(),
                    ),
                    (
                        "Users".into(),
                        if app.config.username.is_empty() {
                            "Not configured".into()
                        } else {
                            app.config.username.clone()
                        },
                    ),
                    ("Install Gentoo".into(), "Review & install".into()),
                ],
                app.main_index,
                "Installation settings",
            )
        };
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, (name, value))| {
            let special = section.is_none() && i == 5;
            let marker = if special { "▶" } else { "›" };
            ListItem::new(vec![
                Line::styled(
                    format!(" {marker} {name}"),
                    Style::default().fg(if special { AMBER } else { FG }),
                ),
                Line::styled(format!("     {value}"), Style::default().fg(MUTED)),
            ])
        })
        .collect();
    let list = List::new(items).block(panel(title)).highlight_style(
        Style::default()
            .bg(PANEL)
            .fg(AMBER)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default().with_selected(Some(index));
    frame.render_stateful_widget(list, area, &mut state);
}
fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    let text = match app.page {
        Page::Main => {
            let summary = match app.main_index {
                0 => "Choose a disk. Stagehand checks existing partition tables and known filesystem signatures without modifying data.",
                1 => "Configure hostname, timezone and console keymap.",
                2 => "Choose the Gentoo kernel package and an EFI bootloader. Both choices affect the actual installation.",
                3 => "Wired DHCP is enabled by default. SSH is optional and requires an authorized public key.",
                4 => "Set a local administrator account and root credentials.",
                _ => "Review every choice, then explicitly authorize installation on the selected disk.",
            };
            format!(
                "Welcome to Stagehand.\n\n{summary}\n\n\
                Installation profile:\n\
                • Native architecture: {}\n\
                • Official Gentoo systemd stage3 (auto-selected)\n\
                • Kernel: {}\n\
                • Bootloader: {}\n\
                • GPT / UEFI, Btrfs @ and @home (zstd)\n\
                • Wired DHCP via systemd-networkd\n\n\
                Enter a category to configure its options.\n\
                Nothing is written to a disk until final confirmation.",
                arch_label(),
                app.config.kernel.label(),
                app.config.bootloader.label()
            )
        }
        Page::Section(section) => {
            let help = match (section, app.section_index) {
                (Section::Storage, _) =>
                    "Inspect disk capacity, partition tables and filesystem signatures. A disk with existing data is clearly marked and still requires explicit ERASE confirmation.",
                (Section::System, 0) => "Choose the installed system's hostname.",
                (Section::System, 1) => "Select an IANA timezone, e.g. Europe/Berlin.",
                (Section::System, _) => "Choose a supported Linux console keymap, e.g. de or us.",
                (Section::Boot, 0) =>
                    "Binary: gentoo-kernel-bin is fast to install (default).\n\nSource: gentoo-kernel builds on the target and may take significantly longer.",
                (Section::Boot, _) =>
                    "systemd-boot is the default for UEFI installs.\n\nGRUB offers a configurable menu and broader boot options. Both require UEFI in this version.",
                (Section::Network, 0) =>
                    "Enable key-only SSH access to the installed system. Password and root SSH login remain disabled.",
                (Section::Network, _) =>
                    "Paste an OpenSSH public key. Required only when SSH is enabled.",
                (Section::Users, 0) => "Set the name of the local administrator.",
                (Section::Users, 1) => "The administrator joins wheel with sudo permission.",
                (Section::Users, _) => "Set a separate root password.",
            };
            format!(
                "{} / {}\n\n{}\n\nCurrent value:\n{}\n\n\
                Enter to edit or select. Esc to return to the main menu.",
                section.title(),
                section.entries()[app.section_index],
                help,
                section_value(app, section, app.section_index)
            )
        }
        Page::Disks => {
            if app.disks.is_empty() {
                "No block disks detected. Check lsblk and device visibility in the live environment.".into()
            } else {
                let items = app
                    .disks
                    .iter()
                    .enumerate()
                    .map(|(i, disk)| {
                        let marker = if i == app.disk_index { "›" } else { " " };
                        let status = disk
                            .block_reason
                            .as_ref()
                            .map(|why| format!("BLOCKED: {why}"))
                            .unwrap_or_else(|| "Selectable".into());
                        format!(
                            "{marker}  {} — {} — {}\n     {}\n     {}",
                            disk.path,
                            disk.size_label(),
                            disk.model,
                            disk.contents.label(),
                            status
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                format!(
                    "Target disk selection\n\n{items}\n\n\
                    Only known partition tables and filesystem signatures can be detected. \
                    No signatures does not guarantee a completely empty disk.\n\
                    WARNING: The entire chosen disk is erased during installation."
                )
            }
        }
        Page::Pick(choice) => {
            let (title, values) = match choice {
                Choice::Kernel => (
                    "Kernel package",
                    [
                        "gentoo-kernel-bin (prebuilt, default)",
                        "gentoo-kernel (source build)",
                    ],
                ),
                Choice::Bootloader => ("UEFI bootloader", ["systemd-boot (default)", "GRUB"]),
            };
            format!(
                "{title}\n\n{}  {}\n{}  {}\n\n\
                Use Up/Down and Enter to select.\n\
                This selection will be used by the installer.",
                if app.choice_index == 0 { "›" } else { " " },
                values[0],
                if app.choice_index == 1 { "›" } else { " " },
                values[1]
            )
        }
        Page::Edit(field) => {
            let value = if field.secret() {
                "●".repeat(app.buffer.chars().count())
            } else {
                app.buffer.clone()
            };
            format!(
                "{}\n\n> {value}█\n\n\
                {}\
                Press Enter to save or Esc to go back.",
                if app.pending_password.is_some() {
                    "Confirm password"
                } else {
                    field.title()
                },
                if field.secret() {
                    "Passwords are entered twice and excluded from logs.\n\n"
                } else {
                    ""
                }
            )
        }
        Page::Confirm => {
            let disk = app.config.disk.as_ref();
            let path = disk.map(|d| d.path.as_str()).unwrap_or("");
            let size = disk.map(|d| d.size_label()).unwrap_or_default();
            let contents = disk.map(|d| d.contents.label()).unwrap_or("Unknown");
            format!(
                "FINAL REVIEW\n\n\
                Architecture: {}\n\
                Stage3:       Gentoo systemd / auto-selected\n\
                Target disk:  {path} ({size})\n\
                Disk state:   {contents}\n\
                Kernel:       {}\n\
                Bootloader:   {}\n\
                Hostname:     {}\n\
                Timezone:     {}\n\
                Keymap:       {}\n\
                User:         {}\n\
                SSH:          {}\n\n\
                WARNING: All data on {path} will be destroyed!\n\
                Type: ERASE {path}\n\n> {}█",
                arch_label(),
                app.config.kernel.label(),
                app.config.bootloader.label(),
                app.config.hostname,
                app.config.timezone,
                app.config.keymap,
                app.config.username,
                if app.config.ssh_enabled {
                    "Key-only"
                } else {
                    "Disabled"
                },
                app.buffer
            )
        }
        Page::Installing => format!(
            "Installing Gentoo Linux\n\nStep {} / {}\n\n{}\n\n\
            Live installation log:\n\n{}",
            app.step,
            install::STEP_COUNT,
            app.current_step,
            log_tail(13)
        ),
        Page::Done => format!(
            "Installation completed.\n\n\
            Remove the installation medium and reboot.\n\n\
            Log: {}\n\n{}",
            install::LOG,
            log_tail(10)
        ),
        Page::Failed => format!(
            "Installation stopped.\n\n{}\n\n\
            Mount cleanup was attempted. Check mount state and logs before retrying.\n\n{}",
            app.install_error,
            log_tail(12)
        ),
    };
    let title = match app.page {
        Page::Main | Page::Section(_) => "Details",
        Page::Disks => "Disk inspection",
        Page::Pick(_) => "Select an option",
        Page::Edit(_) => "Edit value",
        Page::Confirm => "Final confirmation",
        Page::Installing => "Installing",
        Page::Done => "Success",
        Page::Failed => "Error",
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(if app.error.is_empty() { 0 } else { 3 }),
            Constraint::Length(if matches!(app.page, Page::Installing) {
                3
            } else {
                0
            }),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(text)
            .block(panel(title))
            .wrap(Wrap { trim: false }),
        rows[0],
    );
    if !app.error.is_empty() {
        frame.render_widget(
            Paragraph::new(app.error.as_str())
                .style(Style::default().fg(RED))
                .block(panel("Attention")),
            rows[1],
        );
    }
    if matches!(app.page, Page::Installing) {
        frame.render_widget(
            Gauge::default()
                .block(panel("Progress"))
                .gauge_style(Style::default().fg(AQUA).bg(PANEL))
                .ratio((app.step as f64 / install::STEP_COUNT as f64).clamp(0.0, 1.0)),
            rows[2],
        );
    }
}
fn log_tail(lines: usize) -> String {
    let Ok(mut file) = File::open(install::LOG) else {
        return "No log yet.".into();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return String::new();
    };
    let start = len.saturating_sub(8192);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&data)
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

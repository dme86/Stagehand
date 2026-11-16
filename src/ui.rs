use crate::{
    config::{
        Bootloader, BuildJobs, Config, Editor, Filesystem, Kernel, Locale, Network, Stage3, Swap,
    },
    disks::{self, Disk},
    install,
    worker::{self, RunState, RunStatus},
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
    Portage,
    Tools,
}
impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Storage => "Storage",
            Self::System => "System",
            Self::Boot => "Kernel & boot",
            Self::Network => "Network",
            Self::Users => "Users",
            Self::Portage => "Portage",
            Self::Tools => "System tools",
        }
    }
    fn entries(self) -> &'static [&'static str] {
        match self {
            Self::Storage => &["Installation disk", "Root filesystem", "Swap"],
            Self::System => &[
                "Hostname",
                "Timezone",
                "Console keymap",
                "System locale",
                "Stage3 type",
            ],
            Self::Boot => &["Kernel", "Bootloader"],
            Self::Network => &["Network manager", "SSH service", "SSH public key"],
            Self::Users => &["Username", "User password", "Root password"],
            Self::Portage => &["Binary packages", "Build jobs"],
            Self::Tools => &["Editor", "Cron daemon"],
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
    Filesystem,
    Swap,
    Locale,
    Stage3,
    Network,
    BuildJobs,
    Editor,
}
impl Choice {
    fn section(self) -> Section {
        match self {
            Self::Filesystem | Self::Swap => Section::Storage,
            Self::Locale | Self::Stage3 => Section::System,
            Self::Network => Section::Network,
            Self::BuildJobs => Section::Portage,
            Self::Editor => Section::Tools,
            Self::Kernel | Self::Bootloader => Section::Boot,
        }
    }
    fn count(self) -> usize {
        match self {
            Self::Locale | Self::BuildJobs | Self::Editor => 3,
            _ => 2,
        }
    }
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
    status: Option<RunStatus>,
    worker_pid: Option<u32>,
    show_logs: bool,
}
impl App {
    fn new() -> Self {
        let mut app = Self {
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
            status: None,
            worker_pid: None,
            show_logs: false,
        };
        // Re-attach to the last worker without requiring saved credentials.
        if let Ok(Some(snapshot)) = worker::read_status() {
            app.adopt_status(snapshot);
        }
        app
    }
    fn adopt_status(&mut self, mut snapshot: RunStatus) {
        self.worker_pid = Some(snapshot.pid);
        if snapshot.state == RunState::Running && !worker::worker_alive(snapshot.pid) {
            snapshot.state = RunState::Failed;
            snapshot.error = Some("Worker stopped unexpectedly. Check the worker lock and disk mounts before retrying.".into());
        }
        self.page = match snapshot.state {
            RunState::Finished => Page::Done,
            RunState::Failed => Page::Failed,
            RunState::Running => Page::Installing,
        };
        self.status = Some(snapshot);
    }
    fn update(&mut self) {
        if !matches!(self.page, Page::Installing) {
            return;
        }
        match worker::read_status() {
            Ok(Some(snapshot)) => self.adopt_status(snapshot),
            Ok(None) => {
                if self
                    .worker_pid
                    .is_some_and(|pid| !worker::worker_alive(pid))
                {
                    self.page = Page::Failed;
                    self.error = "Worker exited before publishing its status. Check the worker lock and logs.".into();
                }
            }
            Err(e) => {
                self.page = Page::Failed;
                self.error = format!("Cannot read worker status: {e:#}");
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
            Choice::Filesystem => usize::from(self.config.filesystem == Filesystem::Ext4),
            Choice::Swap => usize::from(self.config.swap == Swap::FourGiB),
            Choice::Locale => match self.config.locale {
                Locale::English => 0,
                Locale::German => 1,
                Locale::Utf8 => 2,
            },
            Choice::Stage3 => usize::from(self.config.stage3 == Stage3::Desktop),
            Choice::Network => usize::from(self.config.network == Network::NetworkManager),
            Choice::BuildJobs => match self.config.build_jobs {
                BuildJobs::Auto => 0,
                BuildJobs::Two => 1,
                BuildJobs::Four => 2,
            },
            Choice::Editor => match self.config.editor {
                Editor::None => 0,
                Editor::Nano => 1,
                Editor::Vim => 2,
            },
        };
        self.page = Page::Pick(choice);
        self.error.clear();
    }
    fn section_activate(&mut self, section: Section) {
        self.error.clear();
        match (section, self.section_index) {
            (Section::Storage, 0) => self.open_disks(),
            (Section::Storage, 1) => self.pick(Choice::Filesystem),
            (Section::Storage, 2) => self.pick(Choice::Swap),
            (Section::System, 0) => self.edit(Field::Hostname),
            (Section::System, 1) => self.edit(Field::Timezone),
            (Section::System, 2) => self.edit(Field::Keymap),
            (Section::System, 3) => self.pick(Choice::Locale),
            (Section::System, 4) => self.pick(Choice::Stage3),
            (Section::Boot, 0) => self.pick(Choice::Kernel),
            (Section::Boot, 1) => self.pick(Choice::Bootloader),
            (Section::Network, 0) => self.pick(Choice::Network),
            (Section::Network, 1) => self.config.ssh_enabled = !self.config.ssh_enabled,
            (Section::Network, 2) => self.edit(Field::SshKey),
            (Section::Users, 0) => self.edit(Field::Username),
            (Section::Users, 1) => self.edit(Field::UserPassword),
            (Section::Users, 2) => self.edit(Field::RootPassword),
            (Section::Portage, 0) => self.config.binary_packages = !self.config.binary_packages,
            (Section::Portage, 1) => self.pick(Choice::BuildJobs),
            (Section::Tools, 0) => self.pick(Choice::Editor),
            (Section::Tools, 1) => self.config.cronie = !self.config.cronie,
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
                    self.main_index = (self.main_index + 1).min(7)
                }
                KeyCode::Home => self.main_index = 0,
                KeyCode::End => self.main_index = 7,
                KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => {
                    let section = match self.main_index {
                        0 => Some(Section::Storage),
                        1 => Some(Section::System),
                        2 => Some(Section::Boot),
                        3 => Some(Section::Network),
                        4 => Some(Section::Users),
                        5 => Some(Section::Portage),
                        6 => Some(Section::Tools),
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
                KeyCode::Esc | KeyCode::Left => self.page = Page::Section(choice.section()),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.choice_index = self.choice_index.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.choice_index = (self.choice_index + 1).min(choice.count() - 1);
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
                        Choice::Filesystem => {
                            self.config.filesystem = if self.choice_index == 0 {
                                Filesystem::Btrfs
                            } else {
                                Filesystem::Ext4
                            }
                        }
                        Choice::Swap => {
                            self.config.swap = if self.choice_index == 0 {
                                Swap::None
                            } else {
                                Swap::FourGiB
                            }
                        }
                        Choice::Locale => {
                            self.config.locale = match self.choice_index {
                                0 => Locale::English,
                                1 => Locale::German,
                                _ => Locale::Utf8,
                            }
                        }
                        Choice::Stage3 => {
                            self.config.stage3 = if self.choice_index == 0 {
                                Stage3::Base
                            } else {
                                Stage3::Desktop
                            }
                        }
                        Choice::Network => {
                            self.config.network = if self.choice_index == 0 {
                                Network::Networkd
                            } else {
                                Network::NetworkManager
                            }
                        }
                        Choice::BuildJobs => {
                            self.config.build_jobs = match self.choice_index {
                                0 => BuildJobs::Auto,
                                1 => BuildJobs::Two,
                                _ => BuildJobs::Four,
                            }
                        }
                        Choice::Editor => {
                            self.config.editor = match self.choice_index {
                                0 => Editor::None,
                                1 => Editor::Nano,
                                _ => Editor::Vim,
                            }
                        }
                    }
                    self.page = Page::Section(choice.section());
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
                        match worker::spawn(&self.config) {
                            Ok(pid) => {
                                self.worker_pid = Some(pid);
                                self.status = None;
                                self.show_logs = false;
                                self.page = Page::Installing;
                                self.error.clear();
                                self.buffer.clear();
                                // Credentials no longer needed in the TUI process.
                                self.config.user_password.clear();
                                self.config.root_password.clear();
                            }
                            Err(e) => self.error = format!("Could not start worker: {e:#}"),
                        }
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
            Page::Installing => match key.code {
                KeyCode::Char('l') | KeyCode::Char('L') => self.show_logs = !self.show_logs,
                KeyCode::Char('q') | KeyCode::Esc => return true,
                _ => {}
            },
            Page::Done | Page::Failed => match key.code {
                KeyCode::Char('l') | KeyCode::Char('L') => self.show_logs = !self.show_logs,
                KeyCode::Enter => {
                    self.status = None;
                    self.worker_pid = None;
                    self.error.clear();
                    self.page = Page::Main;
                }
                KeyCode::Esc | KeyCode::Char('q') => return true,
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
            "Gentoo installer  ·  v0.5.3  ·  ",
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
        Page::Installing => " L: Logs / Steps   q/Esc: Detach UI   Worker keeps running",
        Page::Done | Page::Failed => " L: Logs / Steps   Enter: New setup   q/Esc: Quit",
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
        (Section::Storage, 1) => app.config.filesystem.label().into(),
        (Section::Storage, 2) => app.config.swap.label().into(),
        (Section::System, 0) => app.config.hostname.clone(),
        (Section::System, 1) => app.config.timezone.clone(),
        (Section::System, 2) => app.config.keymap.clone(),
        (Section::System, 3) => app.config.locale.label().into(),
        (Section::System, 4) => app.config.stage3.label().into(),
        (Section::Boot, 0) => app.config.kernel.label().into(),
        (Section::Boot, 1) => app.config.bootloader.label().into(),
        (Section::Network, 0) => app.config.network.label().into(),
        (Section::Network, 1) => if app.config.ssh_enabled {
            "Enabled"
        } else {
            "Disabled"
        }
        .into(),
        (Section::Network, 2) => if app.config.ssh_public_key.is_empty() {
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
        (Section::Portage, 0) => if app.config.binary_packages {
            "Prefer binary packages"
        } else {
            "Compile from source"
        }
        .into(),
        (Section::Portage, 1) => app.config.build_jobs.label().into(),
        (Section::Tools, 0) => app.config.editor.label().into(),
        (Section::Tools, 1) => if app.config.cronie {
            "cronie enabled"
        } else {
            "Disabled"
        }
        .into(),
        _ => String::new(),
    }
}
fn render_menu(frame: &mut Frame, area: Rect, app: &App) {
    if matches!(app.page, Page::Installing | Page::Done | Page::Failed) {
        let completed = app.status.as_ref().map_or(0, |s| s.completed);
        let active = app.status.as_ref().map_or(0, |s| s.phase);
        let items: Vec<ListItem> = install::PHASES
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let number = i + 1;
                let (marker, color) = if i < completed {
                    ("✓", AQUA)
                } else if number == active {
                    (
                        "▶",
                        if matches!(app.page, Page::Failed) {
                            RED
                        } else {
                            AMBER
                        },
                    )
                } else {
                    ("·", MUTED)
                };
                ListItem::new(Line::styled(
                    format!(" {marker} {number:>2}. {title}"),
                    Style::default().fg(color),
                ))
            })
            .collect();
        frame.render_widget(List::new(items).block(panel("Installation phases")), area);
        return;
    }
    let section = match app.page {
        Page::Section(section) => Some(section),
        Page::Disks => Some(Section::Storage),
        Page::Pick(choice) => Some(choice.section()),
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
                    (
                        "Portage".into(),
                        if app.config.binary_packages {
                            "Binary packages preferred"
                        } else {
                            "Source builds"
                        }
                        .into(),
                    ),
                    (
                        "System tools".into(),
                        format!("Editor: {}", app.config.editor.label()),
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
            let special = section.is_none() && i == 7;
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
                1 => "Configure hostname, timezone, console keymap, locale and base/desktop Stage3.",
                2 => "Choose the Gentoo kernel package and an EFI bootloader. Both choices affect the actual installation.",
                3 => "Choose systemd-networkd (wired DHCP) or NetworkManager (Wi-Fi-capable). SSH is optional.",
                4 => "Set a local administrator account and root credentials.",
                5 => "Choose binary package preference and parallel build jobs for Portage.",
                6 => "Choose a console editor and optionally enable the cronie scheduler.",
                _ => "Review every choice, then explicitly authorize installation on the selected disk.",
            };
            format!(
                "Welcome to Stagehand.\n\n{summary}\n\n\
                Installation profile:\n\
                • Native architecture: {}\n\
                • Stage3: {} (native architecture)\n\
                • Kernel: {}\n\
                • Bootloader: {}\n\
                • GPT / UEFI, filesystem: {}\n\
                • Swap: {}\n\
                • Locale: {}\n\
                • Network: {}\n\n\
                Enter a category to configure its options.\n\
                Nothing is written to a disk until final confirmation.",
                arch_label(),
                app.config.stage3.label(),
                app.config.kernel.label(),
                app.config.bootloader.label(),
                app.config.filesystem.label(),
                app.config.swap.label(),
                app.config.locale.label(),
                app.config.network.label()
            )
        }
        Page::Section(section) => {
            let help = match (section, app.section_index) {
                (Section::Storage, 0) =>
                    "Inspect disk capacity, partition tables and filesystem signatures. Existing data is clearly marked; erasure requires explicit confirmation.",
                (Section::Storage, 1) =>
                    "Btrfs with zstd and subvolumes @ / @home (default), or ext4 as a single root filesystem.",
                (Section::Storage, _) =>
                    "No swap by default. Optionally create a dedicated 4 GiB swap partition. Requires at least a 16 GiB installation disk.",
                (Section::System, 0) => "Choose the installed system's hostname.",
                (Section::System, 1) => "Select an IANA timezone, e.g. Europe/Berlin.",
                (Section::System, 2) => "Choose a Linux console keymap, e.g. de or us.",
                (Section::System, 3) =>
                    "Choose a UTF-8 system locale: en_US, de_DE or C.UTF-8.",
                (Section::System, _) =>
                    "Base systemd Stage3 (default) or desktop systemd Stage3 with extra developer packages. Desktop Stage3 is not a window manager.",
                (Section::Boot, 0) =>
                    "Binary: gentoo-kernel-bin is fast to install (default).\n\nSource: gentoo-kernel builds on the target and may take significantly longer.",
                (Section::Boot, _) =>
                    "systemd-boot is the default for UEFI installs.\n\nGRUB offers a configurable menu and broader boot options. Both require UEFI in this version.",
                (Section::Network, 0) =>
                    "systemd-networkd provides wired DHCP (default). NetworkManager manages Ethernet and Wi-Fi; Wi-Fi credentials must be configured separately.",
                (Section::Network, 1) =>
                    "Enable SSH with public key authentication. Password/root SSH login stays disabled.",
                (Section::Network, _) =>
                    "Paste an OpenSSH public key. Required only when SSH is enabled.",
                (Section::Users, 0) => "Set the name of the local administrator.",
                (Section::Users, 1) => "The administrator joins wheel with sudo permission.",
                (Section::Users, _) => "Set a separate root password.",
                (Section::Portage, 0) =>
                    "Prefer Gentoo binary packages or compile from source. Preserve the Stage3 profile and binhost.",
                (Section::Portage, _) =>
                    "Use automatic Portage defaults (recommended) or 2/4 build jobs. Allow roughly 2 GiB RAM per parallel compile job.",
                (Section::Tools, 0) =>
                    "Select a console editor: none, nano (default) or vim.",
                (Section::Tools, _) =>
                    "Install/enable cronie for cron jobs, if needed. Systemd timers and built-in journald work without it.",
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
            let (title, options): (&str, &[&str]) = match choice {
                Choice::Kernel => (
                    "Kernel",
                    &[
                        "gentoo-kernel-bin (default)",
                        "gentoo-kernel (build from source)",
                    ],
                ),
                Choice::Bootloader => ("UEFI bootloader", &["systemd-boot (default)", "GRUB"]),
                Choice::Filesystem => (
                    "Root filesystem",
                    &["Btrfs (zstd, @ / @home)", "ext4 (single root)"],
                ),
                Choice::Swap => (
                    "Swap configuration",
                    &["No swap (default)", "4 GiB swap partition"],
                ),
                Choice::Locale => (
                    "System locale",
                    &["en_US.UTF-8 (default)", "de_DE.UTF-8", "C.UTF-8"],
                ),
                Choice::Stage3 => (
                    "Stage3 image",
                    &[
                        "Base systemd (default)",
                        "Desktop systemd (includes extra development packages)",
                    ],
                ),
                Choice::Network => (
                    "Network manager",
                    &[
                        "systemd-networkd (wired DHCP)",
                        "NetworkManager (Ethernet/Wi-Fi)",
                    ],
                ),
                Choice::BuildJobs => (
                    "Parallel builds",
                    &["Automatic (default)", "2 jobs", "4 jobs"],
                ),
                Choice::Editor => ("Console editor", &["None", "nano (default)", "vim"]),
            };
            let options = options
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    format!(
                        "{}  {}",
                        if i == app.choice_index { "›" } else { " " },
                        value
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            format!("{title}\n\n{options}\n\nUse arrows and Enter to choose.")
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
                Stage3:       {}\n\
                Target disk:  {path} ({size})\n\
                Disk state:   {contents}\n\
                Filesystem:   {}\n\
                Swap:         {}\n\
                Kernel:       {}\n\
                Bootloader:   {}\n\
                Hostname:     {}\n\
                Timezone:     {}\n\
                Locale:       {}\n\
                Keymap:       {}\n\
                Network:      {}\n\
                Build jobs:   {}\n\
                Portage:      {}\n\
                Editor:       {}\n\
                Cron:         {}\n\
                User:         {}\n\
                SSH:          {}\n\n\
                WARNING: All data on {path} will be destroyed!\n\
                Type: ERASE {path}\n\n> {}█",
                arch_label(),
                app.config.stage3.label(),
                app.config.filesystem.label(),
                app.config.swap.label(),
                app.config.kernel.label(),
                app.config.bootloader.label(),
                app.config.hostname,
                app.config.timezone,
                app.config.locale.label(),
                app.config.keymap,
                app.config.network.label(),
                app.config.build_jobs.label(),
                if app.config.binary_packages {
                    "Binary preferred"
                } else {
                    "Source builds"
                },
                app.config.editor.label(),
                if app.config.cronie {
                    "cronie enabled"
                } else {
                    "Disabled"
                },
                app.config.username,
                if app.config.ssh_enabled {
                    "Key-only"
                } else {
                    "Disabled"
                },
                app.buffer,
            )
        }
        Page::Installing => {
            let completed = app.status.as_ref().map_or(0, |s| s.completed);
            let phase = app.status.as_ref().map_or(0, |s| s.phase);
            let detail = app
                .status
                .as_ref()
                .map_or("Starting detached worker", |s| s.detail.as_str());
            if app.show_logs {
                format!(
                    "INSTALLATION RUNNING\n\nPhase {phase} / {}  •  {completed} finished\n\
                    {detail}\n\nLive installation log (last lines):\n\n{}",
                    install::STEP_COUNT,
                    log_tail(18)
                )
            } else {
                format!(
                    "INSTALLATION RUNNING\n\n\
                    Current phase: {phase} / {}\n\n\
                    {detail}\n\n\
                    {completed} of {} installation phases completed.\n\n\
                    The installer runs independently from this terminal.\n\
                    Closing Stagehand does NOT interrupt the installation.\n\n\
                    Press L to view the live log.\n\
                    Progress is phase-based, not an estimate of time.\n\n\
                    Worker status: {}",
                    install::STEP_COUNT,
                    install::STEP_COUNT,
                    worker::STATUS_PATH
                )
            }
        }
        Page::Done => {
            if app.show_logs {
                format!(
                    "INSTALLATION COMPLETED\n\n{} / {} phases complete\n\n{}",
                    install::STEP_COUNT,
                    install::STEP_COUNT,
                    log_tail(18)
                )
            } else {
                format!(
                    "INSTALLATION COMPLETED\n\n\
                    {} / {} phases completed.\n\n\
                    The installer finished and unmounted its filesystems.\n\
                    Remove the installation medium and reboot.\n\n\
                    Press L to inspect the installation log.\n\
                    Press Enter to configure another installation.\n\n\
                    Log: {}",
                    install::STEP_COUNT,
                    install::STEP_COUNT,
                    install::LOG
                )
            }
        }
        Page::Failed => {
            let message = app
                .status
                .as_ref()
                .and_then(|s| s.error.as_deref())
                .filter(|s| !s.is_empty())
                .unwrap_or(&app.error);
            if app.show_logs {
                format!(
                    "INSTALLATION FAILED\n\n{message}\n\nLast log lines:\n\n{}",
                    log_tail(17)
                )
            } else {
                format!(
                    "INSTALLATION FAILED\n\n{message}\n\n\
                    The installer attempted to clean up all mounts.\n\
                    Check the log and disk mount state before retrying.\n\n\
                    Press L for the error log or Enter for a new setup.\n\n\
                    Log: {}\nWorker status: {}",
                    install::LOG,
                    worker::STATUS_PATH
                )
            }
        }
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
                .ratio(
                    (app.status.as_ref().map_or(0, |s| s.completed) as f64
                        / install::STEP_COUNT as f64)
                        .clamp(0.0, 1.0),
                )
                .label(format!(
                    "{} / {} phases completed",
                    app.status.as_ref().map_or(0, |s| s.completed),
                    install::STEP_COUNT
                )),
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

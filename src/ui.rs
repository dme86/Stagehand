use crate::{
    config::Config,
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
    fn password(self) -> bool {
        matches!(self, Self::RootPassword | Self::UserPassword)
    }
}
#[derive(Clone, Copy)]
enum Page {
    Main,
    Disks,
    Edit(Field),
    Confirm,
    Installing,
    Done,
    Failed,
}
struct App {
    config: Config,
    page: Page,
    selected: usize,
    disks: Vec<Disk>,
    disk_index: usize,
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
            selected: 0,
            disks: Vec::new(),
            disk_index: 0,
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
        if let Some(rx) = &self.receiver {
            while let Ok(e) = rx.try_recv() {
                match e {
                    Event::Step(n, s) => {
                        self.step = n;
                        self.current_step = s;
                    }
                    Event::Complete(Ok(())) => {
                        self.page = Page::Done;
                    }
                    Event::Complete(Err(e)) => {
                        self.install_error = e;
                        self.page = Page::Failed;
                    }
                }
            }
        }
    }
    fn open_disks(&mut self) {
        match disks::discover() {
            Ok(found) => {
                self.disks = found;
                self.disk_index = 0;
                self.error.clear();
                self.page = Page::Disks;
            }
            Err(e) => self.error = format!("Cannot list disks: {e:#}"),
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
    fn menu_activate(&mut self) {
        self.error.clear();
        match self.selected {
            0 => self.open_disks(),
            1 => self.edit(Field::Hostname),
            2 => self.edit(Field::Timezone),
            3 => self.edit(Field::Keymap),
            4 => self.edit(Field::Username),
            5 => self.edit(Field::UserPassword),
            6 => self.edit(Field::RootPassword),
            7 => self.config.ssh_enabled = !self.config.ssh_enabled,
            8 => self.edit(Field::SshKey),
            9 => match self.config.validate() {
                Ok(()) => {
                    self.buffer.clear();
                    self.page = Page::Confirm;
                }
                Err(e) => self.error = e,
            },
            _ => {}
        }
    }
    fn save_edit(&mut self, field: Field) {
        if field.password() && self.pending_password.is_none() {
            self.pending_password = Some(std::mem::take(&mut self.buffer));
            self.error.clear();
            return;
        }
        if field.password() && self.pending_password.as_deref() != Some(self.buffer.as_str()) {
            self.buffer.clear();
            self.pending_password = None;
            self.error = "Passwords did not match. Enter the password again.".into();
            return;
        }
        let value = if field.password() {
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
        self.page = Page::Main;
        self.error.clear();
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }
        match self.page {
            Page::Main => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return true,
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(9),
                KeyCode::Home => self.selected = 0,
                KeyCode::End => self.selected = 9,
                KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => self.menu_activate(),
                _ => {}
            },
            Page::Disks => match key.code {
                KeyCode::Esc | KeyCode::Left => self.page = Page::Main,
                KeyCode::Up | KeyCode::Char('k') => {
                    self.disk_index = self.disk_index.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.disk_index = (self.disk_index + 1).min(self.disks.len().saturating_sub(1))
                }
                KeyCode::Char('r') => self.open_disks(),
                KeyCode::Enter => {
                    if let Some(d) = self.disks.get(self.disk_index) {
                        self.config.disk = Some(d.clone());
                        self.page = Page::Main;
                    }
                }
                _ => {}
            },
            Page::Edit(field) => match key.code {
                KeyCode::Esc => {
                    self.buffer.clear();
                    self.pending_password = None;
                    self.page = Page::Main;
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
                        self.error.clear();
                        self.page = Page::Installing;
                        self.step = 0;
                        self.current_step = "Starting".into();
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
            Page::Installing => {} // Prevent accidental interruption of disk operations.
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
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
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
                if app.key(key) {
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
fn render(f: &mut Frame, a: &App) {
    let main = f.area();
    f.render_widget(Block::default().style(Style::default().bg(BG)), main);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(main);
    let heading = Paragraph::new(Line::from(vec![
        Span::styled(
            "  STAGEHAND  ",
            Style::default().fg(AMBER).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Gentoo installer  ·  v0.1.0", Style::default().fg(MUTED)),
    ]))
    .block(panel("Welcome"));
    f.render_widget(heading, vertical[0]);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(39), Constraint::Percentage(61)])
        .split(vertical[1]);
    render_menu(f, body[0], a);
    render_content(f, body[1], a);
    let controls = match a.page {
        Page::Main => " ↑↓/j/k Navigate   Enter Edit   q Quit",
        Page::Disks => " ↑↓ Select   Enter Use disk   r Refresh   Esc Back",
        Page::Edit(_) => " Type value   Enter Save   Esc Cancel",
        Page::Confirm => " Type exact ERASE confirmation   Enter Install   Esc Back",
        Page::Installing => " Installation in progress — do not power off",
        Page::Done | Page::Failed => " Enter / Esc Exit   |   Log: /tmp/stagehand-install.log",
    };
    f.render_widget(
        Paragraph::new(controls)
            .style(Style::default().fg(AQUA))
            .block(panel("Keys")),
        vertical[2],
    );
}
fn render_menu(f: &mut Frame, area: Rect, a: &App) {
    let entries = [
        (
            "Storage",
            a.config
                .disk
                .as_ref()
                .map(|d| d.path.clone())
                .unwrap_or_else(|| "Not selected".into()),
        ),
        ("Hostname", a.config.hostname.clone()),
        ("Timezone", a.config.timezone.clone()),
        ("Keymap", a.config.keymap.clone()),
        (
            "User",
            if a.config.username.is_empty() {
                "Not set".into()
            } else {
                a.config.username.clone()
            },
        ),
        (
            "User password",
            if a.config.user_password.is_empty() {
                "Not set"
            } else {
                "Configured"
            }
            .into(),
        ),
        (
            "Root password",
            if a.config.root_password.is_empty() {
                "Not set"
            } else {
                "Configured"
            }
            .into(),
        ),
        (
            "SSH service",
            if a.config.ssh_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
            .into(),
        ),
        (
            "SSH public key",
            if a.config.ssh_public_key.is_empty() {
                "Not set"
            } else {
                "Configured"
            }
            .into(),
        ),
        ("Install Gentoo", "Review & install".into()),
    ];
    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let mark = if i == 9 { "▶" } else { " " };
            ListItem::new(vec![
                Line::styled(
                    format!(" {mark} {label}"),
                    Style::default().fg(if i == 9 { AMBER } else { FG }),
                ),
                Line::styled(format!("     {value}"), Style::default().fg(MUTED)),
            ])
        })
        .collect();
    let list = List::new(items)
        .block(panel("Installation settings"))
        .highlight_style(
            Style::default()
                .bg(PANEL)
                .fg(AMBER)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default().with_selected(Some(a.selected));
    f.render_stateful_widget(list, area, &mut state);
}
fn render_content(f: &mut Frame, area: Rect, a: &App) {
    let text = match a.page {
        Page::Main => {
            let explanations = [
                "Select a physical target disk. Mounted and removable disks are excluded. ALL DATA on the selected disk will be destroyed.",
                "Choose the hostname for your new Gentoo installation.",
                "IANA timezone, e.g. Europe/Berlin or UTC.",
                "Console keymap, e.g. de or us.",
                "Create a non-root administrator account (wheel/sudo).",
                "Enter and confirm the administrator password (minimum eight characters).",
                "Enter and confirm the root password (minimum eight characters).",
                "Toggle SSH. When enabled, public-key authentication is required and root/password SSH logins are disabled.",
                "Paste an OpenSSH public key for the new user when SSH is enabled.",
                "Review the selected disk and explicitly authorize a full-disk installation.",
            ];
            let hint = explanations[a.selected];
            format!("Welcome to Stagehand.\n\n{hint}\n\n\
                Installation profile:\n\
                • Gentoo Linux with systemd\n\
                • GPT / UEFI / systemd-boot\n\
                • Btrfs @ and @home (zstd)\n\
                • Gentoo binary kernel\n\
                • systemd-networkd / DHCP\n\
                • Native amd64 and arm64\n\n\
                Choose each option on the left. No disk changes happen until you confirm the final installation.")
        }
        Page::Disks => {
            if a.disks.is_empty() {
                "No eligible disks found.\n\nCheck lsblk, mounted partitions, active swap, removable devices, and disk size (at least 8 GiB).".into()
            } else {
                let disks = a.disks.iter().enumerate().map(|(i, d)| {
                    let marker = if i == a.disk_index { "›" } else { " " };
                    format!("{marker}  {} — {}\n     {}", d.path, d.size_label(), d.model)
                }).collect::<Vec<_>>().join("\n\n");
                format!("Choose an installation target:\n\n{disks}\n\nWARNING: Stagehand will erase the entire selected disk, including existing partition tables and filesystems.")
            }
        }
        Page::Edit(field) => {
            let value = if field.password() {
                "●".repeat(a.buffer.chars().count())
            } else {
                a.buffer.clone()
            };
            let action = if a.pending_password.is_some() { "Confirm password" }
                else { field.title() };
            format!("{action}\n\n> {value}█\n\n\
                Values are entered locally. Passwords are not printed in installation logs.\n\
                Press Enter to save or Esc to cancel.")
        }
        Page::Confirm => {
            let disk = a.config.disk.as_ref();
            let path = disk.map(|d| d.path.as_str()).unwrap_or("");
            let size = disk.map(|d| d.size_label()).unwrap_or_default();
            format!("FINAL REVIEW\n\nDisk:       {path} ({size})\n\
                Hostname:   {}\nTimezone:   {}\nKeymap:     {}\nUser:       {}\nSSH:        {}\n\n\
                The entire disk will be erased and replaced with GPT, EFI and Btrfs.\n\
                There is NO UNDO.\n\nType: ERASE {path}\n\n> {}█",
                a.config.hostname, a.config.timezone, a.config.keymap,
                a.config.username, if a.config.ssh_enabled { "Key-only" } else { "Off" },
                a.buffer)
        }
        Page::Installing => format!("Installing Gentoo Linux\n\nStep {} / {}\n\n{}\n\n\
            Operations are logged below. The installer will automatically clean up mounts on failure.\n\n\
            Recent output:\n\n{}",
            a.step, install::STEP_COUNT, a.current_step, log_tail(13)),
        Page::Done => format!("Installation completed successfully.\n\n\
            The filesystem has been unmounted. Remove the installation medium and reboot.\n\n\
            Installation log: {}\n\n{}",
            install::LOG, log_tail(10)),
        Page::Failed => format!("Installation stopped due to an error.\n\n{}\n\n\
            The installer attempted to unmount its filesystems. Inspect the log \
            and verify mounts manually before retrying.\n\nRecent output:\n\n{}",
            a.install_error, log_tail(12)),
    };
    let title = match a.page {
        Page::Main => "Details",
        Page::Disks => "Disk selection",
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
            Constraint::Length(if a.error.is_empty() { 0 } else { 3 }),
            Constraint::Length(if matches!(a.page, Page::Installing) {
                3
            } else {
                0
            }),
        ])
        .split(area);
    f.render_widget(
        Paragraph::new(text)
            .block(panel(title))
            .wrap(Wrap { trim: false }),
        rows[0],
    );
    if !a.error.is_empty() {
        f.render_widget(
            Paragraph::new(a.error.as_str())
                .style(Style::default().fg(RED))
                .block(panel("Attention")),
            rows[1],
        );
    }
    if matches!(a.page, Page::Installing) {
        f.render_widget(
            Gauge::default()
                .block(panel("Progress"))
                .gauge_style(Style::default().fg(AQUA).bg(PANEL))
                .ratio((a.step as f64 / install::STEP_COUNT as f64).clamp(0.0, 1.0)),
            rows[2],
        );
    }
}
fn log_tail(lines: usize) -> String {
    let Ok(mut file) = File::open(install::LOG) else {
        return "No log yet.".into();
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return String::new();
    };
    let start = len.saturating_sub(8192);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

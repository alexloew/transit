use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::Backend};

use crate::{
    config::{self, AppConfig},
    local::{self, LocalEntry},
    remote::{self, RemoteEntry},
    transfer::{self, TransferCommand, TransferEvent},
    ui,
};

pub struct Startup {
    pub remote_host: Option<String>,
    pub remote_path: String,
    pub local_dir: PathBuf,
    pub needs_init: bool,
}

impl Startup {
    pub fn from_args(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut args = args.collect::<Vec<_>>();
        let force_init = args
            .first()
            .is_some_and(|arg| arg == "init" || arg == "--init");
        if force_init {
            args.remove(0);
        }

        let loaded = config::load(force_init);
        let remote_host = match args.first() {
            Some(host) => Some(config::sanitize_host(host)?),
            None => Some(loaded.config.remote_host),
        };
        let remote_path = match args.get(1) {
            Some(path) => config::sanitize_remote_path(path)?,
            None => loaded.config.remote_path,
        };

        Ok(Self {
            remote_host,
            remote_path,
            local_dir: loaded.config.local_dir,
            needs_init: loaded.needs_init,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivePane {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMode {
    Normal,
    EditingHost,
    EditingRemotePath,
    EditingLocalPath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewMode {
    Browser,
    Transfers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InitStep {
    RemoteHost,
    RemotePath,
    LocalPath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadStatus {
    Queued,
    Active,
    Done,
    Failed,
    Canceled,
}

#[derive(Clone, Debug)]
pub struct UploadQueueItem {
    pub name: String,
    pub status: UploadStatus,
    pub bytes_sent: Option<u64>,
    pub bytes_total: Option<u64>,
    pub elapsed_secs: u64,
    pub message: Option<String>,
}

impl UploadQueueItem {
    fn queued(path: PathBuf) -> Self {
        let name = display_name(&path);
        Self {
            name,
            status: UploadStatus::Queued,
            bytes_sent: None,
            bytes_total: None,
            elapsed_secs: 0,
            message: None,
        }
    }
}

pub struct App {
    pub local_cwd: PathBuf,
    pub local_entries: Vec<LocalEntry>,
    pub local_selected: usize,
    pub local_marks: HashSet<PathBuf>,
    pub remote_host: String,
    pub remote_cwd: String,
    pub remote_entries: Vec<RemoteEntry>,
    pub remote_selected: usize,
    pub active_pane: ActivePane,
    pub view_mode: ViewMode,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub logs: Vec<String>,
    pub remote_loading: bool,
    pub transfer_queue: Vec<UploadQueueItem>,
    pub transfer_selected: usize,
    pub transfer_status: Option<String>,
    pub transferring: bool,
    pub should_quit: bool,
    init_step: Option<InitStep>,
    remote_rx: Option<Receiver<Result<remote::RemoteListing, String>>>,
    transfer_rx: Option<Receiver<TransferEvent>>,
    transfer_tx: Option<Sender<TransferCommand>>,
}

pub fn run<B>(terminal: &mut Terminal<B>, startup: Startup) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut app = App::new(startup)?;

    loop {
        app.drain_remote_events();
        app.drain_transfer_events();
        terminal.draw(|frame| ui::render(frame, &app))?;

        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(100))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };

            if key.kind == KeyEventKind::Press {
                app.handle_key(key);
            }
        }
    }

    Ok(())
}

impl App {
    pub fn new(startup: Startup) -> Result<Self> {
        let local_cwd = startup.local_dir;
        let local_marks = HashSet::new();
        let local_entries = local::read_dir(&local_cwd, &local_marks)?;
        let remote_host = startup.remote_host.unwrap_or_default();

        let mut app = Self {
            local_cwd,
            local_entries,
            local_selected: 0,
            local_marks,
            remote_host,
            remote_cwd: startup.remote_path,
            remote_entries: Vec::new(),
            remote_selected: 0,
            active_pane: ActivePane::Local,
            view_mode: ViewMode::Browser,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            logs: Vec::new(),
            remote_loading: false,
            transfer_queue: Vec::new(),
            transfer_selected: 0,
            transfer_status: None,
            transferring: false,
            should_quit: false,
            init_step: None,
            remote_rx: None,
            transfer_rx: None,
            transfer_tx: None,
        };

        if startup.needs_init {
            app.begin_init();
        } else if app.remote_host.is_empty() {
            app.begin_edit_host();
            app.log("Enter a remote host, e.g. user@nas or an SSH config alias.");
        } else {
            app.refresh_remote();
        }

        Ok(app)
    }

    pub fn input_title(&self) -> &'static str {
        match self.input_mode {
            InputMode::Normal => "",
            InputMode::EditingHost => "Remote host",
            InputMode::EditingRemotePath => "Remote path",
            InputMode::EditingLocalPath => "Source directory",
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::EditingHost | InputMode::EditingRemotePath | InputMode::EditingLocalPath => {
                self.handle_input_key(key)
            }
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('t') => self.toggle_view_mode(),
            _ if self.view_mode == ViewMode::Transfers => self.handle_transfer_key(key),
            _ => match key.code {
                KeyCode::Tab => self.toggle_active_pane(),
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Enter => self.open_selected(),
                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.open_parent(),
                KeyCode::Char(' ') => self.toggle_mark(),
                KeyCode::Char('u') => self.start_upload(),
                KeyCode::Char('r') => self.refresh_active(),
                KeyCode::Char('o') => self.begin_edit_host(),
                KeyCode::Char('g') => self.begin_edit_remote_path(),
                _ => {}
            },
        }
    }

    fn handle_transfer_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_transfer_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_transfer_selection(1),
            KeyCode::Char('x') | KeyCode::Char('c') => self.cancel_selected_transfer(),
            _ => {}
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.init_step.is_some() {
                    self.init_step = None;
                    self.log("Init cancelled; current settings were not saved.");
                }
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
            }
            KeyCode::Enter => self.commit_input(),
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(char) => {
                if !key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.input_buffer.push(char);
                }
            }
            _ => {}
        }
    }

    fn commit_input(&mut self) {
        let value = self.input_buffer.trim().to_string();
        let original_mode = self.input_mode;

        match self.input_mode {
            InputMode::EditingHost => {
                let host = match config::sanitize_host(&value) {
                    Ok(host) => host,
                    Err(error) => {
                        self.log(format!("Invalid remote host: {error}"));
                        return;
                    }
                };
                self.remote_host = host;
                self.remote_selected = 0;
                if self.init_step == Some(InitStep::RemoteHost) {
                    self.begin_init_remote_path();
                } else {
                    self.refresh_remote();
                }
            }
            InputMode::EditingRemotePath => {
                let path = match config::sanitize_remote_path(&value) {
                    Ok(path) => path,
                    Err(error) => {
                        self.log(format!("Invalid remote path: {error}"));
                        return;
                    }
                };
                self.remote_cwd = path;
                self.remote_selected = 0;
                if self.init_step == Some(InitStep::RemotePath) {
                    self.begin_init_local_path();
                } else {
                    self.refresh_remote();
                }
            }
            InputMode::EditingLocalPath => {
                let local_dir = match config::sanitize_local_dir(&value) {
                    Ok(path) => path,
                    Err(error) => {
                        self.log(format!("Invalid source directory: {error}"));
                        return;
                    }
                };
                self.local_cwd = local_dir;
                self.local_selected = 0;
                self.refresh_local();
                if self.init_step == Some(InitStep::LocalPath) {
                    self.finish_init();
                }
            }
            InputMode::Normal => {}
        }

        if self.input_mode == original_mode {
            self.input_mode = InputMode::Normal;
            self.input_buffer.clear();
        }
    }

    fn toggle_active_pane(&mut self) {
        self.active_pane = match self.active_pane {
            ActivePane::Local => ActivePane::Remote,
            ActivePane::Remote => ActivePane::Local,
        };
    }

    fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Browser => ViewMode::Transfers,
            ViewMode::Transfers => ViewMode::Browser,
        };
    }

    fn move_transfer_selection(&mut self, delta: isize) {
        let len = self.transfer_queue.len();
        if len == 0 {
            self.transfer_selected = 0;
            return;
        }

        self.transfer_selected = (self.transfer_selected as isize + delta)
            .clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    fn cancel_selected_transfer(&mut self) {
        let Some(item) = self.transfer_queue.get_mut(self.transfer_selected) else {
            self.log("No queued upload selected.");
            return;
        };

        if item.status != UploadStatus::Queued {
            self.log("Only queued uploads can be canceled.");
            return;
        }

        item.status = UploadStatus::Canceled;
        item.message = Some("canceled".to_string());
        let name = item.name.clone();
        let index = self.transfer_selected + 1;
        if let Some(tx) = &self.transfer_tx {
            let _ = tx.send(TransferCommand::Cancel(index));
        }
        self.log(format!("Canceled queued item {index}: {name}"));
    }

    fn move_selection(&mut self, delta: isize) {
        let (selected, len) = match self.active_pane {
            ActivePane::Local => (&mut self.local_selected, self.local_entries.len()),
            ActivePane::Remote => (&mut self.remote_selected, self.remote_entries.len()),
        };

        if len == 0 {
            *selected = 0;
            return;
        }

        *selected = (*selected as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize;
    }

    fn open_selected(&mut self) {
        match self.active_pane {
            ActivePane::Local => self.open_local_selected(),
            ActivePane::Remote => self.open_remote_selected(),
        }
    }

    fn open_local_selected(&mut self) {
        let Some(entry) = self.local_entries.get(self.local_selected).cloned() else {
            return;
        };

        if !entry.is_dir {
            return;
        }

        self.local_cwd = entry.path;
        self.local_selected = 0;
        self.refresh_local();
    }

    fn open_remote_selected(&mut self) {
        let Some(entry) = self.remote_entries.get(self.remote_selected).cloned() else {
            return;
        };

        if !entry.is_dir {
            return;
        }

        self.remote_cwd = entry.path;
        self.remote_selected = 0;
        self.refresh_remote();
    }

    fn open_parent(&mut self) {
        match self.active_pane {
            ActivePane::Local => {
                if let Some(parent) = self.local_cwd.parent() {
                    self.local_cwd = parent.to_path_buf();
                    self.local_selected = 0;
                    self.refresh_local();
                }
            }
            ActivePane::Remote => {
                self.remote_cwd = remote::parent_path(&self.remote_cwd);
                self.remote_selected = 0;
                self.refresh_remote();
            }
        }
    }

    fn toggle_mark(&mut self) {
        if self.active_pane != ActivePane::Local {
            return;
        }

        let Some(entry) = self.local_entries.get(self.local_selected) else {
            return;
        };

        if self.local_marks.contains(&entry.path) {
            self.local_marks.remove(&entry.path);
        } else {
            self.local_marks.insert(entry.path.clone());
        }

        self.refresh_local();
    }

    fn start_upload(&mut self) {
        if self.transferring {
            self.log("A transfer is already running.");
            return;
        }

        if self.remote_host.trim().is_empty() {
            self.begin_edit_host();
            self.log("Set a remote host before uploading.");
            return;
        }
        let remote_host = match config::sanitize_host(&self.remote_host) {
            Ok(host) => host,
            Err(error) => {
                self.begin_edit_host();
                self.log(format!("Invalid remote host: {error}"));
                return;
            }
        };
        self.remote_host = remote_host;

        let paths = self.paths_to_upload();
        if paths.is_empty() {
            self.log("Select or mark at least one local file first.");
            return;
        }

        self.log(format!(
            "Queued {} item(s) for upload to {}:{}",
            paths.len(),
            self.remote_host,
            self.remote_cwd
        ));
        self.transfer_queue = paths.iter().cloned().map(UploadQueueItem::queued).collect();
        self.transfer_selected = 0;
        self.transfer_status = None;
        self.view_mode = ViewMode::Transfers;
        let transfer = transfer::upload(paths, self.remote_host.clone(), self.remote_cwd.clone());
        self.transfer_rx = Some(transfer.events);
        self.transfer_tx = Some(transfer.commands);
        self.transferring = true;
    }

    fn paths_to_upload(&self) -> Vec<PathBuf> {
        if !self.local_marks.is_empty() {
            return self
                .local_entries
                .iter()
                .filter(|entry| self.local_marks.contains(&entry.path))
                .map(|entry| entry.path.clone())
                .collect();
        }

        self.local_entries
            .get(self.local_selected)
            .map(|entry| vec![entry.path.clone()])
            .unwrap_or_default()
    }

    fn refresh_active(&mut self) {
        match self.active_pane {
            ActivePane::Local => self.refresh_local(),
            ActivePane::Remote => self.refresh_remote(),
        }
    }

    fn refresh_local(&mut self) {
        match local::read_dir(&self.local_cwd, &self.local_marks) {
            Ok(entries) => {
                self.local_entries = entries;
                self.local_selected =
                    clamp_selection(self.local_selected, self.local_entries.len());
            }
            Err(error) => self.log(format!("Local refresh failed: {error}")),
        }
    }

    fn refresh_remote(&mut self) {
        if self.remote_loading {
            self.log("Remote refresh already running.");
            return;
        }

        let host = match config::sanitize_host(&self.remote_host) {
            Ok(host) => host,
            Err(error) => {
                self.remote_entries.clear();
                self.remote_selected = 0;
                self.begin_edit_host();
                self.log(format!("Invalid remote host: {error}"));
                return;
            }
        };
        self.remote_host = host.clone();
        let path = self.remote_cwd.clone();
        let (tx, rx) = mpsc::channel();

        self.remote_loading = true;
        self.remote_rx = Some(rx);
        self.log(format!("Loading remote {host}:{path}"));

        thread::spawn(move || {
            let result = remote::list_dir(&host, &path).map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
    }

    fn begin_edit_host(&mut self) {
        self.input_mode = InputMode::EditingHost;
        self.input_buffer = self.remote_host.clone();
    }

    fn begin_edit_remote_path(&mut self) {
        self.input_mode = InputMode::EditingRemotePath;
        self.input_buffer = self.remote_cwd.clone();
    }

    fn begin_init(&mut self) {
        self.init_step = Some(InitStep::RemoteHost);
        self.input_mode = InputMode::EditingHost;
        self.input_buffer = self.remote_host.clone();
        self.log("Init: verify the remote SSH host.");
    }

    fn begin_init_remote_path(&mut self) {
        self.init_step = Some(InitStep::RemotePath);
        self.input_mode = InputMode::EditingRemotePath;
        self.input_buffer = self.remote_cwd.clone();
        self.log("Init: verify the remote media path.");
    }

    fn begin_init_local_path(&mut self) {
        self.init_step = Some(InitStep::LocalPath);
        self.input_mode = InputMode::EditingLocalPath;
        self.input_buffer = self.local_cwd.display().to_string();
        self.log("Init: verify the default local source directory.");
    }

    fn finish_init(&mut self) {
        let config = AppConfig {
            remote_host: self.remote_host.clone(),
            remote_path: self.remote_cwd.clone(),
            local_dir: self.local_cwd.clone(),
        };

        match config::save(&config) {
            Ok(()) => {
                self.log(format!(
                    "Init saved to {}.",
                    config::config_path().display()
                ));
                self.init_step = None;
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
                self.refresh_remote();
            }
            Err(error) => self.log(format!("Init save failed: {error}")),
        }
    }

    fn drain_remote_events(&mut self) {
        let Some(rx) = &self.remote_rx else {
            return;
        };

        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("remote worker disconnected".to_string()),
        };

        self.remote_rx = None;
        self.remote_loading = false;

        match result {
            Ok(listing) => {
                self.remote_cwd = listing.cwd;
                self.remote_entries = listing.entries;
                self.remote_selected =
                    clamp_selection(self.remote_selected, self.remote_entries.len());
                self.log("Remote refreshed.");
            }
            Err(error) => {
                self.remote_entries.clear();
                self.remote_selected = 0;
                self.log(format!("Remote refresh failed: {error}"));
            }
        }
    }

    fn drain_transfer_events(&mut self) {
        let mut completed = false;

        loop {
            let Some(rx) = &self.transfer_rx else {
                break;
            };

            match rx.try_recv() {
                Ok(TransferEvent::Canceled { path, index, total }) => {
                    self.update_upload_item(
                        index,
                        UploadStatus::Canceled,
                        None,
                        None,
                        0,
                        Some("canceled".to_string()),
                    );
                    self.transfer_status =
                        Some(format!("Canceled {index}/{total}: {}", display_name(&path)));
                    if index < total {
                        self.log(format!(
                            "Continuing with queued item {}/{}.",
                            index + 1,
                            total
                        ));
                    }
                }
                Ok(TransferEvent::Started {
                    path,
                    index,
                    total,
                    bytes_total,
                }) => {
                    self.update_upload_item(
                        index,
                        UploadStatus::Active,
                        Some(0),
                        bytes_total,
                        0,
                        None,
                    );
                    self.transfer_status = Some(format_transfer_status(
                        "Uploading",
                        index,
                        total,
                        &path,
                        Some(0),
                        bytes_total,
                        0,
                    ));
                    self.log(format!("Starting {index}/{total}: {}", path.display()));
                }
                Ok(TransferEvent::Progress {
                    path,
                    index,
                    total,
                    bytes_sent,
                    bytes_total,
                    elapsed_secs,
                }) => {
                    self.update_upload_item(
                        index,
                        UploadStatus::Active,
                        bytes_sent,
                        bytes_total,
                        elapsed_secs,
                        None,
                    );
                    self.transfer_status = Some(format_transfer_status(
                        "Uploading",
                        index,
                        total,
                        &path,
                        bytes_sent,
                        bytes_total,
                        elapsed_secs,
                    ));
                }
                Ok(TransferEvent::Finished {
                    path,
                    index,
                    total,
                    success,
                    message,
                    bytes_sent,
                    bytes_total,
                    elapsed_secs,
                }) => {
                    let status = if success { "Done" } else { "Failed" };
                    let upload_status = if success {
                        UploadStatus::Done
                    } else {
                        UploadStatus::Failed
                    };
                    self.update_upload_item(
                        index,
                        upload_status,
                        bytes_sent,
                        bytes_total,
                        elapsed_secs,
                        Some(message.clone()),
                    );
                    self.transfer_status = Some(format_transfer_status(
                        status,
                        index,
                        total,
                        &path,
                        bytes_sent,
                        bytes_total,
                        elapsed_secs,
                    ));
                    self.log(format!("{status}: {} ({message})", path.display()));
                    if index < total {
                        self.log(format!(
                            "Continuing with queued item {}/{}.",
                            index + 1,
                            total
                        ));
                    }
                }
                Ok(TransferEvent::Complete) => {
                    completed = true;
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    completed = true;
                    break;
                }
            }
        }

        if completed {
            self.transfer_rx = None;
            self.transfer_tx = None;
            self.transferring = false;
            self.local_marks.clear();
            self.transfer_status = Some("Transfer complete.".to_string());
            self.refresh_local();
            self.refresh_remote();
            self.log("Transfer complete.");
        }
    }

    fn log(&mut self, message: impl Into<String>) {
        self.logs.push(message.into());
        if self.logs.len() > 8 {
            self.logs.remove(0);
        }
    }

    fn update_upload_item(
        &mut self,
        index: usize,
        status: UploadStatus,
        bytes_sent: Option<u64>,
        bytes_total: Option<u64>,
        elapsed_secs: u64,
        message: Option<String>,
    ) {
        let Some(item) = self.transfer_queue.get_mut(index.saturating_sub(1)) else {
            return;
        };

        item.status = status;
        item.bytes_sent = bytes_sent;
        item.bytes_total = bytes_total.or(item.bytes_total);
        item.elapsed_secs = elapsed_secs;
        item.message = message;
    }
}

fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}

fn format_transfer_status(
    action: &str,
    index: usize,
    total: usize,
    path: &Path,
    bytes_sent: Option<u64>,
    bytes_total: Option<u64>,
    elapsed_secs: u64,
) -> String {
    let name = display_name(path);
    match (bytes_sent, bytes_total) {
        (Some(sent), Some(total_bytes)) if total_bytes > 0 => {
            let sent = sent.min(total_bytes);
            let percent = sent as f64 / total_bytes as f64 * 100.0;
            format!(
                "{action} {index}/{total}: {name} - {percent:.1}% ({} / {}, {}s)",
                local::format_size(Some(sent)),
                local::format_size(Some(total_bytes)),
                elapsed_secs
            )
        }
        (_, Some(total_bytes)) if total_bytes > 0 => format!(
            "{action} {index}/{total}: {name} ({} total, {}s)",
            local::format_size(Some(total_bytes)),
            elapsed_secs
        ),
        _ => format!("{action} {index}/{total}: {name} ({}s)", elapsed_secs),
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

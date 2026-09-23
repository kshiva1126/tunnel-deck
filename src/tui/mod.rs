//! Terminal client for daemon-owned forwarding.

use crate::{
    application::hosts::HostCatalog,
    application::import::{
        DirectiveKind, ImportClassification, ImportForwarding, ImportPreview, InvalidReason,
        UnsupportedReason, selected_rules,
    },
    config::{LogLevel, Rule as WireRule, Settings, Theme},
    daemon::lifecycle,
    domain::rule::{Rule, RuleId},
    error::AppError,
    ipc::{Client, DEFAULT_TIMEOUT, Operation, Request, Response},
    platform::{
        CURRENT,
        paths::{Paths, XdgOverrides},
        private_fs::current_uid,
    },
};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event as InputEvent, KeyCode, KeyEvent,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap},
};
use serde_json::{Value, json};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use std::{
    collections::HashSet,
    env, io,
    net::TcpListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use uuid::Uuid;

const LOOPBACK: &str = "127.0.0.1";
const TAB_TITLES: [&str; 5] = ["ダッシュボード", "ホスト", "詳細", "設定", "ヘルプ"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Dashboard,
    Hosts,
    Detail,
    Settings,
    Help,
}

#[derive(Clone, Debug)]
struct RuleView {
    id: Uuid,
    name: String,
    host: String,
    bind_address: String,
    bind_port: u16,
    destination_port: Option<u16>,
    destination_host: Option<String>,
    state: String,
    kind: &'static str,
    auto_start: bool,
    reconnect: bool,
    uptime_seconds: u64,
    reconnect_count: u64,
    last_error: Option<String>,
}
impl RuleView {
    fn from_wire(rule: WireRule) -> Self {
        match rule {
            WireRule::Local {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                destination_host,
                destination_port,
                auto_start,
                reconnect,
                ..
            } => Self {
                id,
                name,
                host: ssh_host_alias,
                bind_address,
                bind_port,
                destination_port: Some(destination_port),
                destination_host: Some(destination_host),
                state: "stopped".into(),
                kind: "local",
                auto_start,
                reconnect,
                uptime_seconds: 0,
                reconnect_count: 0,
                last_error: None,
            },
            WireRule::Remote {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                destination_host,
                destination_port,
                auto_start,
                reconnect,
                ..
            } => Self {
                id,
                name,
                host: ssh_host_alias,
                bind_address,
                bind_port,
                destination_port: Some(destination_port),
                destination_host: Some(destination_host),
                state: "stopped".into(),
                kind: "remote",
                auto_start,
                reconnect,
                uptime_seconds: 0,
                reconnect_count: 0,
                last_error: None,
            },
            WireRule::Dynamic {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                auto_start,
                reconnect,
            } => Self {
                id,
                name,
                host: ssh_host_alias,
                bind_address,
                bind_port,
                destination_port: None,
                destination_host: None,
                state: "stopped".into(),
                kind: "dynamic",
                auto_start,
                reconnect,
                uptime_seconds: 0,
                reconnect_count: 0,
                last_error: None,
            },
        }
    }
    fn url(&self, scheme: &str) -> String {
        let address = if self.bind_address.contains(':') {
            format!("[{}]", self.bind_address)
        } else {
            self.bind_address.clone()
        };
        format!("{scheme}://{address}:{}", self.bind_port)
    }
    fn to_wire(&self) -> WireRule {
        match self.kind {
            "remote" => WireRule::Remote {
                id: self.id,
                name: self.name.clone(),
                ssh_host_alias: self.host.clone(),
                bind_address: self.bind_address.clone(),
                bind_port: self.bind_port,
                destination_host: self.destination_host.clone().unwrap_or_default(),
                destination_port: self.destination_port.unwrap_or_default(),
                auto_start: self.auto_start,
                reconnect: self.reconnect,
            },
            "dynamic" => WireRule::Dynamic {
                id: self.id,
                name: self.name.clone(),
                ssh_host_alias: self.host.clone(),
                bind_address: self.bind_address.clone(),
                bind_port: self.bind_port,
                auto_start: self.auto_start,
                reconnect: self.reconnect,
            },
            _ => WireRule::Local {
                id: self.id,
                name: self.name.clone(),
                ssh_host_alias: self.host.clone(),
                bind_address: self.bind_address.clone(),
                bind_port: self.bind_port,
                destination_host: self.destination_host.clone().unwrap_or_default(),
                destination_port: self.destination_port.unwrap_or_default(),
                auto_start: self.auto_start,
                reconnect: self.reconnect,
            },
        }
    }
}

#[derive(Debug)]
struct ImportPanel {
    host: String,
    preview: Option<ImportPreview>,
    error: Option<String>,
    cursor: usize,
    selected: HashSet<usize>,
    confirming: bool,
}

impl ImportPanel {
    fn preview(preview: ImportPreview) -> Self {
        Self {
            host: preview.ssh_host_alias.clone(),
            preview: Some(preview),
            error: None,
            cursor: 0,
            selected: HashSet::new(),
            confirming: false,
        }
    }
    fn error(host: String, error: String) -> Self {
        Self {
            host,
            preview: None,
            error: Some(error),
            cursor: 0,
            selected: HashSet::new(),
            confirming: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Form {
    id: Uuid,
    editing: bool,
    host: String,
    name: String,
    remote_port: String,
    local_port: String,
    field: usize,
    suggestion: Option<u16>,
    error: Option<String>,
    kind: &'static str,
    auto_start: bool,
    reconnect: bool,
}
impl Form {
    fn new(host: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            editing: false,
            host,
            name: String::new(),
            remote_port: String::new(),
            local_port: String::new(),
            field: 0,
            suggestion: None,
            error: None,
            kind: "local",
            auto_start: false,
            reconnect: false,
        }
    }
    fn edit(rule: &RuleView) -> Self {
        Self {
            id: rule.id,
            editing: true,
            host: rule.host.clone(),
            name: rule.name.clone(),
            remote_port: rule
                .destination_port
                .map(|v| v.to_string())
                .unwrap_or_default(),
            local_port: rule.bind_port.to_string(),
            field: 0,
            suggestion: None,
            error: None,
            kind: rule.kind,
            auto_start: rule.auto_start,
            reconnect: rule.reconnect,
        }
    }
    fn duplicate(rule: &RuleView) -> Self {
        let mut f = Self::edit(rule);
        f.id = Uuid::new_v4();
        f.editing = false;
        f.name.push_str(" copy");
        f
    }
    fn value_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.name,
            1 => &mut self.remote_port,
            _ => &mut self.local_port,
        }
    }
    fn input(&mut self, ch: char) {
        if self.field == 0 || ch.is_ascii_digit() {
            self.value_mut().push(ch)
        }
        if self.field == 1 {
            self.local_port.clone_from(&self.remote_port)
        }
        self.recheck();
    }
    fn backspace(&mut self) {
        self.value_mut().pop();
        if self.field == 1 {
            self.local_port.clone_from(&self.remote_port)
        }
        self.recheck()
    }
    fn recheck(&mut self) {
        self.error = None;
        let Ok(port) = self.local_port.parse::<u16>() else {
            self.suggestion = None;
            return;
        };
        self.suggestion = if port != 0 && !port_available(LOOPBACK, port) {
            next_available_port(LOOPBACK, port)
        } else {
            None
        };
    }
    fn accept_suggestion(&mut self) {
        if let Some(port) = self.suggestion.take() {
            self.local_port = port.to_string()
        }
    }
    fn payload(&mut self) -> Option<Value> {
        let remote = self.remote_port.parse::<u16>().ok().unwrap_or(0);
        let local = self.local_port.parse::<u16>().ok().unwrap_or(0);
        let validation = match self.kind {
            "remote" => Rule::remote(
                RuleId::from_uuid(self.id),
                self.name.clone(),
                self.host.clone(),
                remote,
                LOOPBACK,
                local,
            ),
            "dynamic" => Rule::dynamic(
                RuleId::from_uuid(self.id),
                self.name.clone(),
                self.host.clone(),
                local,
            ),
            _ => Rule::local(
                RuleId::from_uuid(self.id),
                self.name.clone(),
                self.host.clone(),
                local,
                LOOPBACK,
                remote,
            ),
        };
        if let Err(e) = validation {
            self.error = Some(e.to_string());
            return None;
        }
        if self.kind != "remote" && !port_available(LOOPBACK, local) {
            self.suggestion = next_available_port(LOOPBACK, local);
            self.error = Some(format!(
                "ローカルポート {local} は使用中です。候補を選択してください"
            ));
            return None;
        }
        let bind_port = if self.kind == "remote" { remote } else { local };
        let mut value = json!({"kind":self.kind,"id":self.id,"name":self.name,"ssh_host_alias":self.host,"bind_address":LOOPBACK,"bind_port":bind_port,"auto_start":self.auto_start,"reconnect":self.reconnect});
        if self.kind != "dynamic" {
            value["destination_host"] = json!(LOOPBACK);
            value["destination_port"] = json!(if self.kind == "remote" { local } else { remote });
        }
        Some(value)
    }
}

#[derive(Debug)]
struct App {
    page: Page,
    hosts_return: Option<Page>,
    hosts: Vec<String>,
    rules: Vec<RuleView>,
    selected_host: usize,
    selected_rule: usize,
    host_offset: usize,
    rule_offset: usize,
    selected_id: Option<Uuid>,
    form: Option<Form>,
    confirm_delete: Option<Uuid>,
    import: Option<ImportPanel>,
    message: String,
    settings: Settings,
    settings_field: usize,
    quit: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            page: Page::Dashboard,
            hosts_return: None,
            hosts: vec![],
            rules: vec![],
            selected_host: 0,
            selected_rule: 0,
            host_offset: 0,
            rule_offset: 0,
            selected_id: None,
            form: None,
            confirm_delete: None,
            import: None,
            message: "Tab/上部クリック: 画面切替  行クリック/ホイール: 選択  ?: ヘルプ  q: 終了"
                .into(),
            settings: Settings::default(),
            settings_field: 0,
            quit: false,
        }
    }
}
impl App {
    fn show_page(&mut self, page: Page) {
        if self.page == Page::Hosts && page != Page::Hosts {
            self.hosts_return = None;
        }
        self.page = page;
    }

    fn selected(&self) -> Option<&RuleView> {
        self.rules.get(self.selected_rule)
    }
    fn move_selection(&mut self, delta: isize) {
        let (selected, len) = if self.page == Page::Hosts {
            (&mut self.selected_host, self.hosts.len())
        } else {
            (&mut self.selected_rule, self.rules.len())
        };
        if len != 0 {
            *selected = (*selected as isize + delta).clamp(0, len as isize - 1) as usize
        }
        self.selected_id = self.selected().map(|r| r.id)
    }
    fn replace_rules(&mut self, rules: Vec<RuleView>) {
        let keep = self.selected_id.or_else(|| self.selected().map(|r| r.id));
        self.rules = rules;
        self.selected_rule = keep
            .and_then(|id| self.rules.iter().position(|r| r.id == id))
            .unwrap_or_else(|| self.selected_rule.min(self.rules.len().saturating_sub(1)));
        self.selected_id = self.selected().map(|r| r.id)
    }
    fn replace_hosts(&mut self, hosts: Vec<String>) {
        self.hosts = hosts;
        self.selected_host = self.selected_host.min(self.hosts.len().saturating_sub(1));
        self.host_offset = self.host_offset.min(self.selected_host);
    }
}

struct Service {
    socket: PathBuf,
    executable: PathBuf,
}
trait UiService {
    fn call(&self, operation: Operation, payload: Value) -> Result<Value, String>;
    fn load_rules(&self) -> Result<Vec<RuleView>, String>;
    fn load_settings(&self) -> Result<Settings, String>;
    fn save_settings(&self, settings: &Settings) -> Result<Settings, String>;
}
impl Service {
    fn new() -> Result<Self, AppError> {
        let paths = resolved_paths()?;
        let socket = paths
            .socket_path(CURRENT)
            .map_err(|e| AppError::Ipc(e.to_string()))?;
        let executable = env::current_exe().map_err(|e| AppError::Ipc(e.to_string()))?;
        lifecycle::ensure_running(&socket, &executable, DEFAULT_TIMEOUT)
            .map_err(|e| AppError::Ipc(e.to_string()))?;
        Ok(Self { socket, executable })
    }
    fn call_daemon(&self, operation: Operation, payload: Value) -> Result<Value, String> {
        lifecycle::ensure_running(&self.socket, &self.executable, DEFAULT_TIMEOUT)
            .map_err(|e| e.to_string())?;
        let request = Request::new(operation, payload);
        let timeout = if operation == Operation::ForwardStop {
            crate::daemon::process::STOP_RESPONSE_TIMEOUT
        } else {
            DEFAULT_TIMEOUT
        };
        match Client::connect(&self.socket, timeout)
            .and_then(|mut c| c.call(&request))
            .map_err(|e| e.to_string())?
        {
            Response::Success(v) => Ok(v.result),
            Response::Failure(v) => Err(v.error.message),
        }
    }
}
impl UiService for Service {
    fn call(&self, operation: Operation, payload: Value) -> Result<Value, String> {
        self.call_daemon(operation, payload)
    }
    fn load_rules(&self) -> Result<Vec<RuleView>, String> {
        let wire: Vec<WireRule> =
            serde_json::from_value(self.call(Operation::ForwardList, json!({}))?)
                .map_err(|e| e.to_string())?;
        let mut rules: Vec<_> = wire.into_iter().map(RuleView::from_wire).collect();
        if let Ok(status) = self.call(Operation::Status, json!({})) {
            if let Some(states) = status["forwards"].as_array() {
                for state in states {
                    if let (Some(id), Some(value)) = (
                        state["rule_id"]
                            .as_str()
                            .and_then(|v| Uuid::parse_str(v).ok()),
                        state["state"].as_str(),
                    ) {
                        if let Some(rule) = rules.iter_mut().find(|r| r.id == id) {
                            rule.state = value.into();
                            rule.uptime_seconds = state["uptime_seconds"].as_u64().unwrap_or(0);
                            rule.reconnect_count = state["reconnect_count"].as_u64().unwrap_or(0);
                            rule.last_error = state["last_error"].as_str().map(str::to_owned);
                        }
                    }
                }
            }
        }
        Ok(rules)
    }
    fn load_settings(&self) -> Result<Settings, String> {
        serde_json::from_value(self.call(Operation::SettingsGet, json!({}))?)
            .map_err(|error| error.to_string())
    }
    fn save_settings(&self, settings: &Settings) -> Result<Settings, String> {
        let payload = serde_json::to_value(settings).map_err(|error| error.to_string())?;
        serde_json::from_value(self.call(Operation::SettingsUpdate, payload)?)
            .map_err(|error| error.to_string())
    }
}

pub fn run() -> Result<(), AppError> {
    let service = Service::new()?;
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Configuration("HOME is not set".into()))?;
    let mut catalog = HostCatalog::new(home.join(".ssh/config"), home.join(".ssh"), "ssh");
    let mut app = App::default();
    match catalog.refresh() {
        Ok(v) => app.replace_hosts(v.aliases),
        Err(e) => app.message = e.to_string(),
    }
    match service.load_rules() {
        Ok(v) => app.replace_rules(v),
        Err(e) => app.message = e,
    }
    match service.load_settings() {
        Ok(settings) => app.settings = settings,
        Err(error) => app.message = error,
    }
    let stopping = Arc::new(AtomicBool::new(false));
    let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM, SIGHUP])
        .map_err(|e| AppError::Ipc(format!("signal handler: {e}")))?;
    let signal_flag = stopping.clone();
    let handle = signals.handle();
    let signal_thread = thread::spawn(move || {
        if signals.forever().next().is_some() {
            signal_flag.store(true, Ordering::SeqCst)
        }
    });
    let guard = TerminalGuard::enter().map_err(|e| AppError::Ipc(e.to_string()))?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        previous(info)
    }));
    let result = run_loop(&mut app, &service, &mut catalog, stopping);
    drop(guard);
    handle.close();
    let _ = signal_thread.join();
    result.map_err(|e| AppError::Ipc(e.to_string()))
}
fn run_loop(
    app: &mut App,
    service: &Service,
    catalog: &mut HostCatalog,
    stopping: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (tx, rx) = mpsc::channel();
    spawn_subscription(service.socket.clone(), tx);
    while !app.quit && !stopping.load(Ordering::SeqCst) {
        terminal.draw(|f| render(f, app))?;
        if rx.try_recv().is_ok() {
            refresh(app, service)
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        match event::read()? {
            InputEvent::Key(key) => handle_key(app, key, service, catalog),
            InputEvent::Mouse(mouse) => {
                let size = terminal.size()?;
                handle_mouse(app, mouse, Rect::new(0, 0, size.width, size.height));
            }
            _ => {}
        }
    }
    Ok(())
}
fn screen_areas(size: Rect) -> Option<(Rect, Rect, Rect)> {
    if size.width < 42 || size.height < 10 {
        return None;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(size);
    Some((chunks[0], chunks[1], chunks[2]))
}

fn list_offset(selected: usize, visible: usize, offset: usize) -> usize {
    if selected < offset {
        selected
    } else if selected >= offset.saturating_add(visible) {
        selected.saturating_sub(visible.saturating_sub(1))
    } else {
        offset
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent, size: Rect) {
    if app.form.is_some() || app.confirm_delete.is_some() || app.import.is_some() {
        return;
    }
    let Some((tabs, content, _)) = screen_areas(size) else {
        return;
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if mouse.row == tabs.y + 1 && mouse.column > tabs.x && mouse.column < tabs.right() - 1 {
                let mut left = tabs.x + 1;
                for (index, title) in TAB_TITLES.iter().enumerate() {
                    let width = ratatui::text::Line::from(*title).width() as u16
                        + if index + 1 == TAB_TITLES.len() { 2 } else { 3 };
                    let right = left.saturating_add(width).min(tabs.right() - 1);
                    if mouse.column >= left && mouse.column < right {
                        app.show_page(
                            [
                                Page::Dashboard,
                                Page::Hosts,
                                Page::Detail,
                                Page::Settings,
                                Page::Help,
                            ][index],
                        );
                        return;
                    }
                    left = right;
                }
            }
            if mouse.column <= content.x
                || mouse.column >= content.right() - 1
                || mouse.row <= content.y
                || mouse.row >= content.bottom() - 1
            {
                return;
            }
            match app.page {
                Page::Dashboard if mouse.row > content.y + 1 => {
                    let visible = content.height.saturating_sub(3) as usize;
                    let offset = list_offset(app.selected_rule, visible, app.rule_offset);
                    let index = offset + (mouse.row - content.y - 2) as usize;
                    if index < app.rules.len() {
                        app.selected_rule = index;
                        app.selected_id = Some(app.rules[index].id);
                        app.rule_offset = offset;
                    }
                }
                Page::Hosts => {
                    let visible = content.height.saturating_sub(2) as usize;
                    let offset = list_offset(app.selected_host, visible, app.host_offset);
                    let index = offset + (mouse.row - content.y - 1) as usize;
                    if index < app.hosts.len() {
                        app.selected_host = index;
                        app.host_offset = offset;
                    }
                }
                _ => {}
            }
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
            if content.contains((mouse.column, mouse.row).into()) =>
        {
            if matches!(app.page, Page::Dashboard | Page::Hosts) {
                app.move_selection(if mouse.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                });
                match app.page {
                    Page::Dashboard => {
                        let visible = content.height.saturating_sub(3) as usize;
                        app.rule_offset = list_offset(app.selected_rule, visible, app.rule_offset);
                    }
                    Page::Hosts => {
                        let visible = content.height.saturating_sub(2) as usize;
                        app.host_offset = list_offset(app.selected_host, visible, app.host_offset);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
fn spawn_subscription(socket: PathBuf, sender: mpsc::Sender<()>) {
    thread::spawn(move || {
        let Ok(client) = Client::connect(&socket, DEFAULT_TIMEOUT) else {
            return;
        };
        let Ok(mut subscription) = client.subscribe() else {
            return;
        };
        while subscription.read_event().is_ok() {
            if sender.send(()).is_err() {
                break;
            }
        }
    });
}
fn refresh(app: &mut App, service: &dyn UiService) {
    match service.load_rules() {
        Ok(v) => app.replace_rules(v),
        Err(e) => app.message = e,
    }
    if let Ok(settings) = service.load_settings() {
        app.settings = settings;
    }
}

fn rule_snapshots(app: &App) -> Result<(Vec<Rule>, Vec<RuleId>), String> {
    let rules = app
        .rules
        .iter()
        .map(|view| Rule::try_from(view.to_wire()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let running = app
        .rules
        .iter()
        .filter(|view| view.state != "stopped")
        .map(|view| RuleId::from_uuid(view.id))
        .collect();
    Ok((rules, running))
}

fn open_import(app: &mut App, catalog: &HostCatalog, host: String) {
    let panel = match rule_snapshots(app).and_then(|(rules, running)| {
        catalog
            .import_preview(&host, &rules, &running)
            .map_err(|error| error.to_string())
    }) {
        Ok(preview) => ImportPanel::preview(preview),
        Err(error) => ImportPanel::error(host, error),
    };
    app.import = Some(panel);
}

fn save_import(app: &mut App, service: &dyn UiService) {
    let Some(panel) = app.import.as_ref() else {
        return;
    };
    let Some(preview) = panel.preview.clone() else {
        return;
    };
    let selected = panel.selected.clone();
    let (existing, _) = match rule_snapshots(app) {
        Ok(value) => value,
        Err(error) => {
            app.message = error;
            return;
        }
    };
    let settings = match service.load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            app.message = error.clone();
            if let Some(panel) = &mut app.import {
                panel.confirming = false;
                panel.error = Some(error);
            }
            return;
        }
    };
    let rules = match selected_rules(
        &preview,
        &selected,
        &existing,
        settings.default_auto_start,
        settings.default_reconnect,
    ) {
        Ok(rules) => rules.iter().map(WireRule::from).collect::<Vec<_>>(),
        Err(error) => {
            app.message = error;
            return;
        }
    };
    match service.call(Operation::ForwardImport, json!({"rules": rules})) {
        Ok(_) => {
            app.import = None;
            app.message = format!("選択した {} 件を保存しました（未起動）", selected.len());
            refresh(app, service);
        }
        Err(error) => {
            if let Some(panel) = &mut app.import {
                panel.confirming = false;
                panel.error = Some(error);
            }
        }
    }
}

fn handle_import_key(app: &mut App, key: KeyEvent, service: &dyn UiService) {
    let mut save = false;
    let mut close = false;
    if let Some(panel) = &mut app.import {
        if panel.confirming {
            match key.code {
                KeyCode::Enter | KeyCode::Char('y') => save = true,
                KeyCode::Esc | KeyCode::Char('n') => panel.confirming = false,
                _ => {}
            }
        } else {
            let len = panel
                .preview
                .as_ref()
                .map_or(0, |preview| preview.candidates.len());
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Down | KeyCode::Char('j') if len != 0 => {
                    panel.cursor = (panel.cursor + 1).min(len - 1)
                }
                KeyCode::Up | KeyCode::Char('k') if len != 0 => {
                    panel.cursor = panel.cursor.saturating_sub(1)
                }
                KeyCode::Char(' ') if len != 0 => {
                    let id = panel.cursor + 1;
                    let selectable = panel.preview.as_ref().is_some_and(|preview| {
                        matches!(
                            preview.candidates[panel.cursor].classification,
                            ImportClassification::Supported
                        )
                    });
                    if selectable && !panel.selected.remove(&id) {
                        panel.selected.insert(id);
                    }
                }
                KeyCode::Enter if !panel.selected.is_empty() => panel.confirming = true,
                _ => {}
            }
        }
    }
    if close {
        app.import = None;
    }
    if save {
        save_import(app, service);
    }
}

fn handle_key(app: &mut App, key: KeyEvent, service: &dyn UiService, catalog: &mut HostCatalog) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    if app.import.is_some() {
        handle_import_key(app, key, service);
        return;
    }
    if let Some(id) = app.confirm_delete {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                match service.call(Operation::ForwardRemove, json!({"rule_id":id})) {
                    Ok(_) => {
                        app.message = "転送を削除しました".into();
                        refresh(app, service)
                    }
                    Err(e) => app.message = e,
                }
                app.confirm_delete = None
            }
            KeyCode::Char('n') | KeyCode::Esc => app.confirm_delete = None,
            _ => {}
        }
        return;
    }
    if let Some(form) = &mut app.form {
        match key.code {
            KeyCode::Esc => app.form = None,
            KeyCode::Tab | KeyCode::Down => form.field = (form.field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => form.field = (form.field + 2) % 3,
            KeyCode::Backspace => form.backspace(),
            KeyCode::Char('a') if form.field != 0 && form.suggestion.is_some() => {
                form.accept_suggestion()
            }
            KeyCode::F(2) => {
                form.kind = match form.kind {
                    "local" => "remote",
                    "remote" => "dynamic",
                    _ => "local",
                }
            }
            KeyCode::F(3) => form.auto_start = !form.auto_start,
            KeyCode::F(4) => form.reconnect = !form.reconnect,
            KeyCode::Char(ch) => form.input(ch),
            KeyCode::Enter => {
                if let Some(payload) = form.payload() {
                    match service.call(Operation::ForwardAdd, payload) {
                        Ok(v) => {
                            let id = v["rule_id"]
                                .as_str()
                                .and_then(|v| Uuid::parse_str(v).ok())
                                .unwrap_or(form.id);
                            match service.call(Operation::ForwardStart, json!({"rule_id":id})) {
                                Ok(_) => {
                                    app.message = "転送を保存して起動しました".into();
                                    app.form = None;
                                    refresh(app, service)
                                }
                                Err(e) => {
                                    form.error = Some(format!("保存済みですが起動できません: {e}"))
                                }
                            }
                        }
                        Err(e) => form.error = Some(e),
                    }
                }
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.show_page(Page::Help),
        KeyCode::Esc if app.page == Page::Hosts => {
            if let Some(previous) = app.hosts_return.take() {
                app.show_page(previous);
            }
        }
        KeyCode::Tab => {
            let next = match app.page {
                Page::Dashboard => Page::Hosts,
                Page::Hosts => Page::Detail,
                Page::Detail => Page::Settings,
                Page::Settings => Page::Help,
                Page::Help => Page::Dashboard,
            };
            app.show_page(next);
        }
        KeyCode::Down | KeyCode::Char('j') if app.page == Page::Settings => {
            app.settings_field = (app.settings_field + 1) % 4
        }
        KeyCode::Up | KeyCode::Char('k') if app.page == Page::Settings => {
            app.settings_field = (app.settings_field + 3) % 4
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ')
            if app.page == Page::Settings =>
        {
            let previous = app.settings.clone();
            match app.settings_field {
                0 => {
                    app.settings.theme = match app.settings.theme {
                        Theme::System => Theme::Dark,
                        Theme::Dark => Theme::Light,
                        Theme::Light => Theme::System,
                    }
                }
                1 => {
                    app.settings.log_level = match app.settings.log_level {
                        LogLevel::Error => LogLevel::Warn,
                        LogLevel::Warn => LogLevel::Info,
                        LogLevel::Info => LogLevel::Debug,
                        LogLevel::Debug => LogLevel::Trace,
                        LogLevel::Trace => LogLevel::Error,
                    }
                }
                2 => app.settings.default_reconnect = !app.settings.default_reconnect,
                _ => app.settings.default_auto_start = !app.settings.default_auto_start,
            }
            match service.save_settings(&app.settings) {
                Ok(settings) => {
                    app.settings = settings;
                    app.message = "設定を保存しました".into();
                }
                Err(error) => {
                    app.settings = previous;
                    app.message = error;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Enter if app.page == Page::Hosts => {
            if let Some(host) = app.hosts.get(app.selected_host).cloned() {
                let mut form = Form::new(host);
                form.auto_start = app.settings.default_auto_start;
                form.reconnect = app.settings.default_reconnect;
                app.form = Some(form)
            }
        }
        KeyCode::Enter if app.page == Page::Dashboard => app.show_page(Page::Detail),
        KeyCode::Char('n') => {
            if app.page != Page::Hosts {
                app.hosts_return = Some(app.page);
            }
            app.page = Page::Hosts;
            app.message = "ホストを選び Enter を押してください".into()
        }
        KeyCode::Char('r') if app.page == Page::Hosts => match catalog.refresh() {
            Ok(v) => {
                app.replace_hosts(v.aliases);
                app.message = "ホスト一覧を更新しました".into()
            }
            Err(e) => app.message = e.to_string(),
        },
        KeyCode::Char('i') if app.page == Page::Hosts => {
            if let Some(host) = app.hosts.get(app.selected_host).cloned() {
                open_import(app, catalog, host);
            }
        }
        KeyCode::Char('i') if matches!(app.page, Page::Dashboard | Page::Detail) => {
            if let Some(host) = app.selected().map(|rule| rule.host.clone()) {
                open_import(app, catalog, host);
            }
        }
        KeyCode::Char('e') => {
            if let Some(rule) = app.selected().cloned() {
                if rule.state == "stopped" {
                    app.form = Some(Form::edit(&rule))
                } else {
                    app.message = "編集前に転送を停止してください".into()
                }
            }
        }
        KeyCode::Char('c') => {
            if let Some(rule) = app.selected().cloned() {
                app.form = Some(Form::duplicate(&rule))
            }
        }
        KeyCode::Char('d') => {
            if let Some(rule) = app.selected() {
                app.confirm_delete = Some(rule.id)
            }
        }
        KeyCode::Char(' ') => {
            if let Some(rule) = app.selected().cloned() {
                let op = if rule.state == "stopped" {
                    Operation::ForwardStart
                } else {
                    Operation::ForwardStop
                };
                match service.call(op, json!({"rule_id":rule.id})) {
                    Ok(_) => refresh(app, service),
                    Err(e) => app.message = e,
                }
            }
        }
        KeyCode::Char('h') if app.page == Page::Detail => open_selected(app, "http"),
        KeyCode::Char('s') if app.page == Page::Detail => open_selected(app, "https"),
        _ => {}
    }
}
fn open_selected(app: &mut App, scheme: &str) {
    if let Some(rule) = app.selected() {
        let url = rule.url(scheme);
        app.message = match crate::platform::browser::open(&url) {
            Ok(()) => format!("ブラウザで {url} を開きました"),
            Err(e) => format!("ブラウザを起動できません: {e}"),
        }
    }
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();
    if area.width < 42 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("TunnelDeck\n端末が小さすぎます\n42x10 以上に広げてください\nq: 終了")
                .block(Block::default().borders(Borders::ALL)),
            area,
        );
        return;
    }
    let Some((tabs_area, content, footer)) = screen_areas(area) else {
        return;
    };
    let titles = TAB_TITLES.into_iter().map(Line::from).collect::<Vec<_>>();
    let selected = match app.page {
        Page::Dashboard => 0,
        Page::Hosts => 1,
        Page::Detail => 2,
        Page::Settings => 3,
        Page::Help => 4,
    };
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .block(Block::default().title(" TunnelDeck ").borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        tabs_area,
    );
    match app.page{Page::Dashboard=>render_dashboard(frame,content,app),Page::Hosts=>render_hosts(frame,content,app),Page::Detail=>render_detail(frame,content,app),Page::Settings=>render_settings(frame,content,app),Page::Help=>frame.render_widget(Paragraph::new("j/k・↑/↓/ホイール 選択  行クリック 選択のみ  Enter 詳細/決定  Space 起動/停止\nn 新規  e 編集  c 複製  d 削除  r ホスト更新  i SSH転送import\nEsc nから開いたホスト一覧から戻る（フォーム・importでは閉じる）\nh HTTP  s HTTPS  Tab/上部クリック 画面切替  q 終了（転送は継続）").wrap(Wrap{trim:false}).block(Block::default().title("ヘルプ").borders(Borders::ALL)),content)}
    let hint = page_hint(app, footer.width);
    frame.render_widget(Paragraph::new(format!("{hint}\n{}", app.message)), footer);
    if let Some(form) = &app.form {
        render_form(frame, area, form)
    }
    if app.confirm_delete.is_some() {
        render_confirm(frame, area)
    }
    if let Some(import) = &app.import {
        render_import(frame, area, import)
    }
}
fn page_hint(app: &App, width: u16) -> &'static str {
    let (full, compact) = match app.page {
        Page::Dashboard => (
            "n: 新規  ↑/↓: 選択  Enter: 詳細  ?: ヘルプ",
            "n: 新規  Enter: 詳細  ?: ヘルプ",
        ),
        Page::Hosts if app.hosts_return.is_some() => (
            "↑/↓: 選択  Enter: 新規  Esc: 戻る  ?: ヘルプ",
            "Enter: 新規  Esc: 戻る  ?: ヘルプ",
        ),
        Page::Hosts => (
            "↑/↓: 選択  Enter: 新規  Tab: 切替  ?: ヘルプ",
            "Enter: 新規  Tab: 切替  ?: ヘルプ",
        ),
        _ => return "",
    };
    if Line::from(full).width() <= usize::from(width) {
        full
    } else {
        compact
    }
}
fn render_settings(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let marker = |field| {
        if app.settings_field == field {
            ">"
        } else {
            " "
        }
    };
    let text = format!(
        "{} テーマ: {:?}\n{} ログレベル: {:?}\n{} 新規ルールの再接続: {}\n{} 新規ルールの自動起動: {}\n\n↑/↓: 項目選択  Enter/Space/←/→: 変更して保存",
        marker(0),
        app.settings.theme,
        marker(1),
        app.settings.log_level,
        marker(2),
        app.settings.default_reconnect,
        marker(3),
        app.settings.default_auto_start
    );
    frame.render_widget(
        Paragraph::new(text).block(Block::default().title("設定").borders(Borders::ALL)),
        area,
    );
}
fn render_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let offset = list_offset(
        app.selected_rule,
        area.height.saturating_sub(3) as usize,
        app.rule_offset,
    );
    let rows = app.rules.iter().enumerate().skip(offset).map(|(i, r)| {
        Row::new(vec![
            Cell::from(if i == app.selected_rule { ">" } else { " " }),
            Cell::from(r.name.clone()),
            Cell::from(r.host.clone()),
            Cell::from(r.state.clone()),
            Cell::from(format!("{}:{}", r.bind_address, r.bind_port)),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Length(10),
            Constraint::Min(16),
        ],
    )
    .header(
        Row::new(["", "名前", "ホスト", "状態", "ローカルアドレス"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(format!(
                "転送 {}件 / 稼働 {}件",
                app.rules.len(),
                app.rules.iter().filter(|r| r.state == "active").count()
            ))
            .borders(Borders::ALL),
    );
    frame.render_widget(table, area)
}
fn render_hosts(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let offset = list_offset(
        app.selected_host,
        area.height.saturating_sub(2) as usize,
        app.host_offset,
    );
    let items = if app.hosts.is_empty() {
        vec![ListItem::new("SSH config に具体的な Host がありません")]
    } else {
        app.hosts
            .iter()
            .enumerate()
            .skip(offset)
            .map(|(i, h)| {
                ListItem::new(format!(
                    "{} {h}",
                    if i == app.selected_host { ">" } else { " " }
                ))
            })
            .collect()
    };
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("Enter: ポート追加  i: SSH転送import")
                .borders(Borders::ALL),
        ),
        area,
    )
}
fn candidate_text(candidate: &crate::application::import::ImportCandidate) -> String {
    let kind = match candidate.kind {
        DirectiveKind::Local => "Local",
        DirectiveKind::Remote => "Remote",
        DirectiveKind::Dynamic => "Dynamic",
    };
    let endpoint = match &candidate.forwarding {
        Some(ImportForwarding::Local {
            bind_address,
            bind_port,
            destination_host,
            destination_port,
        })
        | Some(ImportForwarding::Remote {
            bind_address,
            bind_port,
            destination_host,
            destination_port,
        }) => format!("{bind_address}:{bind_port} -> {destination_host}:{destination_port}"),
        Some(ImportForwarding::Dynamic {
            bind_address,
            bind_port,
        }) => format!("{bind_address}:{bind_port}"),
        None => "-".to_owned(),
    };
    let status = match &candidate.classification {
        ImportClassification::Supported => "保存可能".to_owned(),
        ImportClassification::DuplicateDirective { first_index } => {
            format!("重複: 候補 {}", first_index + 1)
        }
        ImportClassification::DuplicateRule { running, .. } => {
            format!("重複: 保存済み{}", if *running { "・実行中" } else { "" })
        }
        ImportClassification::Conflict { running, .. } => {
            format!("競合{}", if *running { "・実行中" } else { "" })
        }
        ImportClassification::Unsupported { reason } => format!(
            "未対応: {}",
            match reason {
                UnsupportedReason::UnixSocket => "Unix socket",
                UnsupportedReason::RemoteDynamic => "Remote SOCKS",
            }
        ),
        ImportClassification::Invalid { reason } => format!(
            "無効: {}",
            match reason {
                InvalidReason::Syntax => "構文",
                InvalidReason::BindAddress => "bind",
                InvalidReason::DestinationHost => "宛先",
                InvalidReason::Port => "port",
                InvalidReason::NonUtf8Output => "非UTF-8",
            }
        ),
    };
    format!("{kind:<7} {endpoint}  [{status}]")
}
fn render_import(frame: &mut ratatui::Frame<'_>, area: Rect, import: &ImportPanel) {
    let popup = centered(area, 90, 18);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title("SSH転送 import preview")
        .borders(Borders::ALL);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let mut footer = Vec::new();
    if let Some(error) = &import.error {
        footer.push(Line::from(Span::styled(
            format!("エラー: {error}"),
            Style::default().fg(Color::Red),
        )));
    }
    footer.push(Line::from(if import.confirming {
        format!(
            "選択した {} 件だけ保存します。Enter/y: 保存  Esc/n: 戻る",
            import.selected.len()
        )
    } else {
        "Space: 選択  Enter: 最終確認  Esc: キャンセル（変更なし）".to_owned()
    }));
    let footer_height = footer.len().min(inner.height.saturating_sub(2) as usize) as u16;
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(footer_height),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(format!("ホスト: {}", import.host)),
        sections[0],
    );

    let mut candidates = Vec::new();
    if let Some(preview) = &import.preview {
        if preview.candidates.is_empty() {
            candidates.push(Line::from("転送候補はありません"));
        }
        for (index, candidate) in preview.candidates.iter().enumerate() {
            let cursor = if index == import.cursor { ">" } else { " " };
            let mark = if import.selected.contains(&(index + 1)) {
                "[x]"
            } else if matches!(candidate.classification, ImportClassification::Supported) {
                "[ ]"
            } else {
                "[-]"
            };
            candidates.push(Line::from(format!(
                "{cursor}{mark} {}. {}",
                index + 1,
                candidate_text(candidate)
            )));
        }
    }
    let visible_candidates = sections[1].height as usize;
    let scroll = import
        .cursor
        .saturating_add(1)
        .saturating_sub(visible_candidates)
        .min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(candidates).scroll((scroll, 0)), sections[1]);
    frame.render_widget(Paragraph::new(footer), sections[2]);
}
fn render_detail(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let text=app.selected().map(|r|format!("名前: {}\n種別: {}\nホスト: {}\n状態: {}\n待受: {}:{} / 宛先ポート: {}\n稼働時間: {}秒 / 再接続: {}回\n最終診断: {}\nURL: {}\n\nh: HTTPで開く  s: HTTPSで開く",r.name,r.kind,r.host,r.state,r.bind_address,r.bind_port,r.destination_port.map(|v|v.to_string()).unwrap_or_else(||"-".into()),r.uptime_seconds,r.reconnect_count,r.last_error.as_deref().unwrap_or("-"),r.url("http"))).unwrap_or_else(||"転送がありません".into());
    frame.render_widget(
        Paragraph::new(text).block(Block::default().title("詳細").borders(Borders::ALL)),
        area,
    )
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn render_form(frame: &mut ratatui::Frame<'_>, area: Rect, form: &Form) {
    let popup = centered(area, 64, 13);
    frame.render_widget(Clear, popup);
    let marker = |f| if form.field == f { ">" } else { " " };
    let mut lines = vec![
        Line::from(format!("ホスト: {}", form.host)),
        Line::from(format!("種別: {} (F2で変更)", form.kind)),
        Line::from(format!("{} 名前: {}", marker(0), form.name)),
        Line::from(format!(
            "{} リモートポート: {}",
            marker(1),
            form.remote_port
        )),
        Line::from(format!("{} ローカルポート: {}", marker(2), form.local_port)),
        Line::from(format!(
            "auto_start: {} (F3) / reconnect: {} (F4)",
            form.auto_start, form.reconnect
        )),
        Line::from("Tab: 項目移動  Enter: 保存して起動  Esc: キャンセル"),
    ];
    if let Some(port) = form.suggestion {
        lines.push(Line::from(Span::styled(
            format!("ポート使用中。候補 {port} (a で選択)"),
            Style::default().fg(Color::Yellow),
        )))
    }
    if let Some(error) = &form.error {
        lines.push(Line::from(Span::styled(
            error,
            Style::default().fg(Color::Red),
        )))
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .title(if form.editing {
                        "転送を編集"
                    } else {
                        "ポートを追加"
                    })
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: false }),
        popup,
    )
}
fn render_confirm(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let popup = centered(area, 46, 5);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new("この転送を削除しますか？\ny/Enter: 削除  n/Esc: 戻る")
            .block(Block::default().title("削除確認").borders(Borders::ALL)),
        popup,
    )
}

fn port_available(address: &str, port: u16) -> bool {
    port != 0 && TcpListener::bind((address, port)).is_ok()
}
fn next_available_port(address: &str, port: u16) -> Option<u16> {
    (port.saturating_add(1)..=u16::MAX)
        .take(100)
        .find(|candidate| port_available(address, *candidate))
}
fn resolved_paths() -> Result<Paths, AppError> {
    let home = env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Configuration("HOME is not set".into()))?;
    let get = |name| {
        env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let config = get("XDG_CONFIG_HOME");
    let state = get("XDG_STATE_HOME");
    let runtime = get("XDG_RUNTIME_DIR");
    Paths::resolve(
        CURRENT,
        &home,
        current_uid(),
        XdgOverrides {
            config: config.as_deref(),
            state: state.as_deref(),
            runtime: runtime.as_deref(),
        },
    )
    .map_err(|e| AppError::Configuration(e.to_string()))
}
struct TerminalGuard;
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(e) = execute!(
            io::stdout(),
            EnterAlternateScreen,
            crossterm::cursor::Hide,
            EnableMouseCapture
        ) {
            restore_terminal();
            return Err(e);
        }
        Ok(Self)
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal()
    }
}
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = write_terminal_restore(&mut io::stdout());
}
fn write_terminal_restore(writer: &mut impl io::Write) -> io::Result<()> {
    execute!(
        writer,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockService {
        calls: RefCell<Vec<(Operation, Value)>>,
        rules: RefCell<Vec<RuleView>>,
        import_error: Option<String>,
        settings_error: Option<String>,
    }
    impl UiService for MockService {
        fn call(&self, operation: Operation, payload: Value) -> Result<Value, String> {
            self.calls.borrow_mut().push((operation, payload));
            if operation == Operation::ForwardImport {
                if let Some(error) = &self.import_error {
                    return Err(error.clone());
                }
                return Ok(json!({"rule_ids": []}));
            }
            Ok(json!({}))
        }
        fn load_rules(&self) -> Result<Vec<RuleView>, String> {
            Ok(self.rules.borrow().clone())
        }
        fn load_settings(&self) -> Result<Settings, String> {
            self.settings_error
                .clone()
                .map_or_else(|| Ok(Settings::default()), Err)
        }
        fn save_settings(&self, settings: &Settings) -> Result<Settings, String> {
            Ok(settings.clone())
        }
    }
    fn import_preview() -> ImportPreview {
        crate::application::import::preview_effective_forwards(
            "dev",
            b"localforward 127.0.0.1:3000 127.0.0.1:30\nremoteforward 127.0.0.1:4000 127.0.0.1:40\ndynamicforward 127.0.0.1:1080\nlocalforward 127.0.0.1:3000 127.0.0.1:30\nlocalforward 127.0.0.1:3000 127.0.0.1:31\nremoteforward 9000\n",
            &[],
            &[],
        ).unwrap()
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }
    fn sample_rule() -> RuleView {
        RuleView {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "dev".into(),
            bind_address: LOOPBACK.into(),
            bind_port: 3000,
            destination_port: Some(3000),
            destination_host: Some(LOOPBACK.into()),
            state: "stopped".into(),
            kind: "local",
            auto_start: false,
            reconnect: false,
            uptime_seconds: 0,
            reconnect_count: 0,
            last_error: None,
        }
    }

    fn rendered(app: &App, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 20)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .replace(' ', "")
    }

    #[test]
    fn new_hosts_escape_returns_to_origin_after_closing_overlays_without_mutation() {
        let service = MockService::default();
        let mut catalog = HostCatalog::new(
            PathBuf::from("/nonexistent/config"),
            PathBuf::from("/nonexistent/ssh"),
            "ssh",
        );
        let mut app = App {
            hosts: vec!["dev".into()],
            rules: vec![sample_rule()],
            ..App::default()
        };
        handle_key(&mut app, key(KeyCode::Char('n')), &service, &mut catalog);
        assert_eq!(app.page, Page::Hosts);
        assert_eq!(app.hosts_return, Some(Page::Dashboard));
        handle_key(&mut app, key(KeyCode::Enter), &service, &mut catalog);
        assert!(app.form.is_some());
        handle_key(&mut app, key(KeyCode::Esc), &service, &mut catalog);
        assert!(app.form.is_none());
        assert_eq!(app.page, Page::Hosts);
        app.import = Some(ImportPanel::preview(import_preview()));
        handle_key(&mut app, key(KeyCode::Esc), &service, &mut catalog);
        assert!(app.import.is_none());
        assert_eq!(app.page, Page::Hosts);
        handle_key(&mut app, key(KeyCode::Esc), &service, &mut catalog);
        assert_eq!(app.page, Page::Dashboard);
        assert_eq!(app.hosts_return, None);
        assert!(service.calls.borrow().is_empty());

        app.show_page(Page::Detail);
        handle_key(&mut app, key(KeyCode::Char('n')), &service, &mut catalog);
        handle_key(&mut app, key(KeyCode::Char('n')), &service, &mut catalog);
        handle_key(&mut app, key(KeyCode::Esc), &service, &mut catalog);
        assert_eq!(app.page, Page::Detail);
        assert!(service.calls.borrow().is_empty());
    }

    #[test]
    fn leaving_hosts_by_tab_clears_the_escape_destination() {
        let service = MockService::default();
        let mut catalog = HostCatalog::new(
            PathBuf::from("/nonexistent/config"),
            PathBuf::from("/nonexistent/ssh"),
            "ssh",
        );
        let mut app = App::default();
        handle_key(&mut app, key(KeyCode::Char('n')), &service, &mut catalog);
        handle_key(&mut app, key(KeyCode::Tab), &service, &mut catalog);
        assert_eq!(app.hosts_return, None);
        app.show_page(Page::Hosts);
        handle_key(&mut app, key(KeyCode::Esc), &service, &mut catalog);
        assert_eq!(app.page, Page::Hosts);
        assert!(service.calls.borrow().is_empty());
    }

    #[test]
    fn dashboard_hosts_and_help_show_discoverable_keys_at_normal_and_narrow_widths() {
        let mut app = App::default();
        for width in [80, 42] {
            let screen = rendered(&app, width);
            assert!(screen.contains("n:新規"), "{width}: {screen}");
            assert!(screen.contains("Enter:詳細"), "{width}: {screen}");
            assert!(screen.contains("?:ヘルプ"), "{width}: {screen}");
        }
        app.show_page(Page::Hosts);
        let screen = rendered(&app, 42);
        assert!(screen.contains("Tab:切替"), "{screen}");
        assert!(!screen.contains("Esc:戻る"), "{screen}");
        app.hosts_return = Some(Page::Dashboard);
        for width in [80, 42] {
            let screen = rendered(&app, width);
            assert!(screen.contains("Enter:新規"), "{width}: {screen}");
            assert!(screen.contains("Esc:戻る"), "{width}: {screen}");
            assert!(screen.contains("?:ヘルプ"), "{width}: {screen}");
        }
        let service = MockService::default();
        let mut catalog = HostCatalog::new(
            PathBuf::from("/nonexistent/config"),
            PathBuf::from("/nonexistent/ssh"),
            "ssh",
        );
        handle_key(&mut app, key(KeyCode::Char('?')), &service, &mut catalog);
        assert_eq!(app.page, Page::Help);
        let screen = rendered(&app, 80);
        assert!(screen.contains("Escnから開いたホスト一覧から戻る"));
    }

    #[test]
    fn mouse_tabs_select_pages_and_ignore_borders_and_popup() {
        let mut app = App::default();
        let size = Rect::new(0, 0, 80, 20);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 19, 1),
            size,
        );
        assert_eq!(app.page, Page::Hosts);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 0, 1),
            size,
        );
        assert_eq!(app.page, Page::Hosts);
        app.form = Some(Form::new("dev".into()));
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 3, 1),
            size,
        );
        assert_eq!(app.page, Page::Hosts);
        app.form = None;
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 3, 1),
            Rect::new(0, 0, 41, 20),
        );
        assert_eq!(app.page, Page::Hosts);
    }

    #[test]
    fn mouse_rows_only_select_and_wheel_moves_visible_selection() {
        let mut app = App::default();
        app.replace_rules((0..20).map(|_| sample_rule()).collect());
        app.hosts = (0..20).map(|i| format!("host-{i}")).collect();
        let size = Rect::new(0, 0, 80, 12);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 6),
            size,
        );
        assert_eq!(app.selected_rule, 1);
        assert_eq!(app.selected_id, Some(app.rules[1].id));
        assert!(app.form.is_none() && app.confirm_delete.is_none() && !app.quit);
        for _ in 0..10 {
            handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 5, 5), size);
        }
        assert_eq!(app.selected_rule, 11);
        let offset = list_offset(app.selected_rule, 4, app.rule_offset);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 5),
            size,
        );
        assert_eq!(app.selected_rule, offset);
        assert_eq!(app.rule_offset, offset, "click must preserve the viewport");
        assert_eq!(list_offset(app.selected_rule, 4, app.rule_offset), offset);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 4),
            size,
        );
        assert_eq!(app.selected_rule, offset);
        app.page = Page::Hosts;
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 5, 6),
            size,
        );
        assert_eq!(app.selected_host, 2);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 5, 5), size);
        assert_eq!(app.selected_host, 1);
        assert!(app.form.is_none() && app.confirm_delete.is_none());
    }

    #[test]
    fn mouse_ignores_empty_rows_and_scroll_keeps_selection_visible_after_resize() {
        let mut app = App::default();
        let size = Rect::new(0, 0, 42, 10);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 5),
            size,
        );
        assert!(app.selected_id.is_none());
        app.hosts = (0..20).map(|i| format!("host-{i}")).collect();
        app.page = Page::Hosts;
        for _ in 0..19 {
            handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 2, 4), size);
        }
        assert_eq!(app.selected_host, 19);
        let backend = TestBackend::new(42, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            rendered.contains("> host-19"),
            "selected host is hidden: {rendered}"
        );
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 8),
            size,
        );
        assert_eq!(app.selected_host, 19, "footer click must be ignored");
    }

    #[test]
    fn host_refresh_clamps_selection_and_viewport() {
        let mut app = App {
            hosts: (0..20).map(|i| format!("host-{i}")).collect(),
            selected_host: 19,
            host_offset: 16,
            ..App::default()
        };
        app.replace_hosts(vec!["remaining".into()]);
        assert_eq!(app.selected_host, 0);
        assert_eq!(app.host_offset, 0);
        assert_eq!(list_offset(app.selected_host, 4, app.host_offset), 0);
        app.replace_hosts(Vec::new());
        assert_eq!(app.selected_host, 0);
        assert_eq!(app.host_offset, 0);
    }

    #[test]
    fn import_saves_only_explicit_supported_selection_after_confirmation_without_starting() {
        let service = MockService::default();
        let mut app = App {
            import: Some(ImportPanel::preview(import_preview())),
            ..App::default()
        };
        handle_import_key(&mut app, key(KeyCode::Char(' ')), &service);
        handle_import_key(&mut app, key(KeyCode::Down), &service);
        handle_import_key(&mut app, key(KeyCode::Down), &service);
        handle_import_key(&mut app, key(KeyCode::Char(' ')), &service);
        handle_import_key(&mut app, key(KeyCode::Enter), &service);
        assert!(
            service.calls.borrow().is_empty(),
            "confirmation must not save"
        );
        handle_import_key(&mut app, key(KeyCode::Enter), &service);

        let calls = service.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, Operation::ForwardImport);
        let rules = calls[0].1["rules"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["kind"], "local");
        assert_eq!(rules[1]["kind"], "dynamic");
        assert!(app.import.is_none());
    }

    #[test]
    fn import_cancel_and_save_failure_leave_the_preview_without_other_mutations() {
        let service = MockService::default();
        let mut app = App {
            import: Some(ImportPanel::preview(import_preview())),
            ..App::default()
        };
        handle_import_key(&mut app, key(KeyCode::Esc), &service);
        assert!(app.import.is_none());
        assert!(service.calls.borrow().is_empty());

        let service = MockService {
            import_error: Some("save failed".into()),
            ..MockService::default()
        };
        app.import = Some(ImportPanel::preview(import_preview()));
        handle_import_key(&mut app, key(KeyCode::Char(' ')), &service);
        handle_import_key(&mut app, key(KeyCode::Enter), &service);
        handle_import_key(&mut app, key(KeyCode::Enter), &service);
        let panel = app.import.as_ref().expect("failed save keeps preview open");
        assert_eq!(panel.error.as_deref(), Some("save failed"));
        assert!(!panel.confirming);
        assert_eq!(
            service
                .calls
                .borrow()
                .iter()
                .filter(|(op, _)| *op == Operation::ForwardImport)
                .count(),
            1
        );

        let service = MockService {
            settings_error: Some("settings unavailable".into()),
            ..MockService::default()
        };
        app.import = Some(ImportPanel::preview(import_preview()));
        handle_import_key(&mut app, key(KeyCode::Char(' ')), &service);
        handle_import_key(&mut app, key(KeyCode::Enter), &service);
        handle_import_key(&mut app, key(KeyCode::Enter), &service);
        let panel = app
            .import
            .as_ref()
            .expect("settings failure keeps preview open");
        assert_eq!(panel.error.as_deref(), Some("settings unavailable"));
        assert!(service.calls.borrow().is_empty());
    }

    #[test]
    fn import_renders_normal_empty_error_narrow_and_short_states() {
        let states = [
            ImportPanel::preview(import_preview()),
            ImportPanel::preview(ImportPreview {
                ssh_host_alias: "empty".into(),
                candidates: vec![],
            }),
            ImportPanel::error("dev".into(), "query failed".into()),
        ];
        for panel in states {
            let app = App {
                import: Some(panel),
                ..App::default()
            };
            let backend = TestBackend::new(100, 30);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            if app
                .import
                .as_ref()
                .is_some_and(|panel| panel.host == "dev" && panel.preview.is_some())
            {
                for expected in ["Local", "Remote", "Dynamic"] {
                    assert!(text.contains(expected), "missing {expected}: {text}");
                }
                let preview = app.import.as_ref().unwrap().preview.as_ref().unwrap();
                assert!(candidate_text(&preview.candidates[3]).contains("重複"));
                assert!(candidate_text(&preview.candidates[4]).contains("競合"));
                assert!(candidate_text(&preview.candidates[5]).contains("未対応"));
            }
        }
        for (width, height) in [(50, 12), (100, 10), (41, 30), (100, 9)] {
            let app = App {
                import: Some(ImportPanel::preview(import_preview())),
                ..App::default()
            };
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
        }
    }

    #[test]
    fn import_scrolls_long_candidate_lists_while_keeping_controls_visible() {
        let output = (1..=24)
            .map(|port| format!("localforward 127.0.0.1:{} 127.0.0.1:80", 3000 + port))
            .collect::<Vec<_>>()
            .join("\n");
        let preview = crate::application::import::preview_effective_forwards(
            "dev",
            output.as_bytes(),
            &[],
            &[],
        )
        .unwrap();
        let mut panel = ImportPanel::preview(preview);
        panel.cursor = 23;
        let app = App {
            import: Some(panel),
            ..App::default()
        };
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(
            text.contains(">[ ] 24."),
            "cursor candidate is hidden: {text}"
        );
        assert!(text.contains("Esc:"), "controls are hidden: {text}");
    }

    #[test]
    fn remote_port_becomes_local_default_and_busy_port_needs_acceptance() {
        let listener = TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut form = Form::new("dev".into());
        form.field = 1;
        for ch in port.to_string().chars() {
            form.input(ch)
        }
        assert_eq!(form.local_port, port.to_string());
        assert!(form.suggestion.is_some());
        assert!(form.payload().is_none());
        form.accept_suggestion();
        assert!(form.payload().is_none(), "name remains required")
    }
    #[test]
    fn remote_form_maps_remote_listener_to_local_destination() {
        let mut form = Form::new("dev".into());
        form.kind = "remote";
        form.name = "callback".into();
        form.remote_port = "9000".into();
        form.local_port = "3000".into();

        let payload = form.payload().expect("valid remote forwarding");

        assert_eq!(payload["bind_port"], 9000);
        assert_eq!(payload["destination_port"], 3000);
    }
    #[test]
    fn suggestion_shortcut_does_not_consume_a_in_the_name_field() {
        let listener = TcpListener::bind((LOOPBACK, 0)).unwrap();
        let mut app = App::default();
        let mut form = Form::new("dev".into());
        form.local_port = listener.local_addr().unwrap().port().to_string();
        form.suggestion = Some(3001);
        app.form = Some(form);
        let service = Service {
            socket: PathBuf::new(),
            executable: PathBuf::new(),
        };
        let mut catalog = HostCatalog::new(PathBuf::new(), PathBuf::new(), "ssh");

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &service,
            &mut catalog,
        );

        let form = app.form.expect("form remains open");
        assert_eq!(form.name, "a");
        assert!(form.suggestion.is_some());
    }
    #[test]
    fn selection_survives_event_refresh_by_rule_id() {
        let make = |id, name: &str| RuleView {
            id,
            name: name.into(),
            host: "dev".into(),
            bind_address: LOOPBACK.into(),
            bind_port: 3000,
            destination_port: Some(3000),
            destination_host: Some(LOOPBACK.into()),
            state: "stopped".into(),
            kind: "local",
            auto_start: false,
            reconnect: false,
            uptime_seconds: 0,
            reconnect_count: 0,
            last_error: None,
        };
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut app = App::default();
        app.replace_rules(vec![make(first, "a"), make(second, "b")]);
        app.selected_rule = 1;
        app.selected_id = Some(second);
        app.replace_rules(vec![make(second, "b"), make(first, "a")]);
        assert_eq!(app.selected_rule, 0)
    }
    #[test]
    fn renders_normal_narrow_short_and_empty_states() {
        for (width, height) in [(100, 30), (41, 30), (100, 9), (42, 10)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render(frame, &App::default()))
                .unwrap();
        }
    }
    #[test]
    fn settings_page_renders_the_daemon_values() {
        let mut app = App {
            page: Page::Settings,
            ..App::default()
        };
        app.settings.theme = Theme::Dark;
        app.settings.log_level = LogLevel::Debug;
        app.settings.default_reconnect = true;
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Dark"));
        assert!(text.contains("Debug"));
    }
    #[test]
    fn ipv6_urls_are_bracketed() {
        let rule = RuleView {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "dev".into(),
            bind_address: "::1".into(),
            bind_port: 3000,
            destination_port: Some(3000),
            destination_host: Some(LOOPBACK.into()),
            state: "active".into(),
            kind: "local",
            auto_start: false,
            reconnect: false,
            uptime_seconds: 1,
            reconnect_count: 0,
            last_error: None,
        };
        assert_eq!(rule.url("https"), "https://[::1]:3000")
    }
    #[test]
    fn cleanup_emits_leave_alternate_screen_and_show_cursor() {
        let mut bytes = Vec::new();
        write_terminal_restore(&mut bytes).unwrap();
        assert!(bytes.windows(8).any(|part| part == b"\x1b[?1006l"));
        assert!(bytes.windows(8).any(|part| part == b"\x1b[?1000l"));
        assert!(bytes.windows(7).any(|part| part == b"?1049l\x1b"));
        assert!(bytes.ends_with(b"\x1b[?25h"));
    }
}

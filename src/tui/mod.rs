//! Terminal client for daemon-owned forwarding.

use crate::{
    application::hosts::HostCatalog,
    config::Rule as WireRule,
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
    event::{self, Event as InputEvent, KeyCode, KeyEvent, KeyModifiers},
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Dashboard,
    Hosts,
    Detail,
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
    state: String,
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
                destination_port,
                ..
            }
            | WireRule::Remote {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                destination_port,
                ..
            } => Self {
                id,
                name,
                host: ssh_host_alias,
                bind_address,
                bind_port,
                destination_port: Some(destination_port),
                state: "stopped".into(),
            },
            WireRule::Dynamic {
                id,
                name,
                ssh_host_alias,
                bind_address,
                bind_port,
                ..
            } => Self {
                id,
                name,
                host: ssh_host_alias,
                bind_address,
                bind_port,
                destination_port: None,
                state: "stopped".into(),
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
        if let Err(e) = Rule::local(
            RuleId::from_uuid(self.id),
            self.name.clone(),
            self.host.clone(),
            local,
            LOOPBACK,
            remote,
        ) {
            self.error = Some(e.to_string());
            return None;
        }
        if !port_available(LOOPBACK, local) {
            self.suggestion = next_available_port(LOOPBACK, local);
            self.error = Some(format!(
                "ローカルポート {local} は使用中です。候補を選択してください"
            ));
            return None;
        }
        Some(
            json!({"kind":"local","id":self.id,"name":self.name,"ssh_host_alias":self.host,"bind_address":LOOPBACK,"bind_port":local,"destination_host":LOOPBACK,"destination_port":remote,"auto_start":false,"reconnect":false}),
        )
    }
}

#[derive(Debug)]
struct App {
    page: Page,
    hosts: Vec<String>,
    rules: Vec<RuleView>,
    selected_host: usize,
    selected_rule: usize,
    selected_id: Option<Uuid>,
    form: Option<Form>,
    confirm_delete: Option<Uuid>,
    message: String,
    quit: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            page: Page::Dashboard,
            hosts: vec![],
            rules: vec![],
            selected_host: 0,
            selected_rule: 0,
            selected_id: None,
            form: None,
            confirm_delete: None,
            message: "Tab: 画面切替  ?: ヘルプ  q: 終了".into(),
            quit: false,
        }
    }
}
impl App {
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
}

struct Service {
    socket: PathBuf,
    executable: PathBuf,
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
    fn call(&self, operation: Operation, payload: Value) -> Result<Value, String> {
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
                            rule.state = value.into()
                        }
                    }
                }
            }
        }
        Ok(rules)
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
        Ok(v) => app.hosts = v.aliases,
        Err(e) => app.message = e.to_string(),
    }
    match service.load_rules() {
        Ok(v) => app.replace_rules(v),
        Err(e) => app.message = e,
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
        if let InputEvent::Key(key) = event::read()? {
            handle_key(app, key, service, catalog)
        }
    }
    Ok(())
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
fn refresh(app: &mut App, service: &Service) {
    match service.load_rules() {
        Ok(v) => app.replace_rules(v),
        Err(e) => app.message = e,
    }
}

fn handle_key(app: &mut App, key: KeyEvent, service: &Service, catalog: &mut HostCatalog) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.quit = true;
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
            KeyCode::Char('a') if form.suggestion.is_some() => form.accept_suggestion(),
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
        KeyCode::Char('?') => app.page = Page::Help,
        KeyCode::Tab => {
            app.page = match app.page {
                Page::Dashboard => Page::Hosts,
                Page::Hosts => Page::Detail,
                Page::Detail => Page::Help,
                Page::Help => Page::Dashboard,
            }
        }
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Enter if app.page == Page::Hosts => {
            if let Some(host) = app.hosts.get(app.selected_host).cloned() {
                app.form = Some(Form::new(host))
            }
        }
        KeyCode::Enter if app.page == Page::Dashboard => app.page = Page::Detail,
        KeyCode::Char('n') => {
            app.page = Page::Hosts;
            app.message = "ホストを選び Enter を押してください".into()
        }
        KeyCode::Char('r') if app.page == Page::Hosts => match catalog.refresh() {
            Ok(v) => {
                app.hosts = v.aliases;
                app.message = "ホスト一覧を更新しました".into()
            }
            Err(e) => app.message = e.to_string(),
        },
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(area);
    let titles = ["ダッシュボード", "ホスト", "詳細", "ヘルプ"]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let selected = match app.page {
        Page::Dashboard => 0,
        Page::Hosts => 1,
        Page::Detail => 2,
        Page::Help => 3,
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
        chunks[0],
    );
    match app.page{Page::Dashboard=>render_dashboard(frame,chunks[1],app),Page::Hosts=>render_hosts(frame,chunks[1],app),Page::Detail=>render_detail(frame,chunks[1],app),Page::Help=>frame.render_widget(Paragraph::new("j/k・↑/↓ 選択  Enter 詳細/決定  Space 起動/停止\nn 新規  e 編集  c 複製  d 削除  r ホスト更新\nh HTTP  s HTTPS  Tab 画面切替  q 終了（転送は継続）").wrap(Wrap{trim:false}).block(Block::default().title("ヘルプ").borders(Borders::ALL)),chunks[1])}
    frame.render_widget(Paragraph::new(app.message.as_str()), chunks[2]);
    if let Some(form) = &app.form {
        render_form(frame, area, form)
    }
    if app.confirm_delete.is_some() {
        render_confirm(frame, area)
    }
}
fn render_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows = app.rules.iter().enumerate().map(|(i, r)| {
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
    let items = if app.hosts.is_empty() {
        vec![ListItem::new("SSH config に具体的な Host がありません")]
    } else {
        app.hosts
            .iter()
            .enumerate()
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
                .title("ホストを選択し Enter → ポート追加")
                .borders(Borders::ALL),
        ),
        area,
    )
}
fn render_detail(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let text=app.selected().map(|r|format!("名前: {}\nホスト: {}\n状態: {}\nLocal: {}:{} → 127.0.0.1:{}\nURL: {}\n\nh: HTTPで開く  s: HTTPSで開く",r.name,r.host,r.state,r.bind_address,r.bind_port,r.destination_port.unwrap_or(0),r.url("http"))).unwrap_or_else(||"転送がありません".into());
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
        Line::from(format!("{} 名前: {}", marker(0), form.name)),
        Line::from(format!(
            "{} リモートポート: {}",
            marker(1),
            form.remote_port
        )),
        Line::from(format!("{} ローカルポート: {}", marker(2), form.local_port)),
        Line::from("Local / remote 127.0.0.1 / local 127.0.0.1"),
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
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen, crossterm::cursor::Hide) {
            let _ = disable_raw_mode();
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
    execute!(writer, LeaveAlternateScreen, crossterm::cursor::Show)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
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
    fn selection_survives_event_refresh_by_rule_id() {
        let make = |id, name: &str| RuleView {
            id,
            name: name.into(),
            host: "dev".into(),
            bind_address: LOOPBACK.into(),
            bind_port: 3000,
            destination_port: Some(3000),
            state: "stopped".into(),
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
    fn ipv6_urls_are_bracketed() {
        let rule = RuleView {
            id: Uuid::new_v4(),
            name: "web".into(),
            host: "dev".into(),
            bind_address: "::1".into(),
            bind_port: 3000,
            destination_port: Some(3000),
            state: "active".into(),
        };
        assert_eq!(rule.url("https"), "https://[::1]:3000")
    }
    #[test]
    fn cleanup_emits_leave_alternate_screen_and_show_cursor() {
        let mut bytes = Vec::new();
        write_terminal_restore(&mut bytes).unwrap();
        assert!(bytes.windows(7).any(|part| part == b"?1049l\x1b"));
        assert!(bytes.ends_with(b"\x1b[?25h"));
    }
}

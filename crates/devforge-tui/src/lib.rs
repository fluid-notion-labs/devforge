//! devforge-tui: ratatui front-end. Pure IPC client — verbs + state stream
//! over the daemon socket. Nothing here touches engine internals.

use std::collections::HashMap;
use std::io::{self, Stdout};
use std::path::Path;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tokio::sync::mpsc;

use devforge_core::client::{Client, Logs, ScenarioStatus};
use devforge_core::ipc::{Reply, StreamEvent, Verb};
use devforge_core::state::ServiceState;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io: {0}")]
    Io(#[from] io::Error),
    #[error("engine socket: {0}")]
    Engine(String),
}

fn status_name(state: ServiceState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{state:?}"))
}

/// Everything the UI renders; updated from the IPC state stream.
pub struct TuiModel {
    pub profile: String,
    pub services: Vec<ServiceRow>,
    pub selected: usize,
    pub quit: bool,
    pub logs: HashMap<String, Vec<String>>,
    pub message: Option<String>,
}

#[derive(Clone)]
pub struct ServiceRow {
    pub name: String,
    pub port: Option<u16>,
    pub state: ServiceState,
}

impl Default for TuiModel {
    fn default() -> Self {
        Self {
            profile: "—".into(),
            services: Vec::new(),
            selected: 0,
            quit: false,
            logs: HashMap::new(),
            message: None,
        }
    }
}

impl TuiModel {
    fn apply_status(&mut self, status: ScenarioStatus) {
        self.profile = status
            .profile
            .unwrap_or_else(|| format!("none [{}]", status.name));
        self.services = status
            .services
            .into_iter()
            .map(|s| ServiceRow {
                name: s.name.clone(),
                port: s.port,
                state: s.state,
            })
            .collect();
        self.selected = self.selected.min(self.services.len().saturating_sub(1));
    }

    fn apply_stream(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Transition(t) => {
                if let Some(row) = self.services.iter_mut().find(|s| s.name == t.service) {
                    row.state = t.to;
                }
            }
            StreamEvent::BuildSignal { service, signal } => {
                self.logs.entry(service).or_default().push(signal);
                self.logs.iter_mut().for_each(|(_, lines)| {
                    if lines.len() > 1000 {
                        lines.drain(..lines.len() - 1000);
                    }
                });
            }
        }
    }
}

/// Run the TUI against the daemon socket at `socket_path` (see DEVFORGE_SOCKET).
pub async fn run(socket_path: &Path) -> Result<(), TuiError> {
    let mut client = Client::connect(socket_path)
        .await
        .map_err(|e| TuiError::Engine(e.to_string()))?;

    // Stream events arrive on their own connection.
    let (net_tx, mut net_rx) = mpsc::unbounded_channel();
    let sub_socket = socket_path.to_path_buf();
    tokio::spawn(async move {
        // Result intentionally ignored: errors surface when the daemon leaves.
        let _ = Client::subscribe(sub_socket, net_tx).await;
    });

    let mut model = TuiModel::default();
    match client.status().await {
        Ok(status) => model.apply_status(status),
        Err(e) => model.message = Some(format!("status: {e}")),
    }

    let mut terminal = setup()?;
    let res = event_loop(&mut terminal, &mut model, &mut client, &mut net_rx).await;
    teardown()?;
    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    model: &mut TuiModel,
    client: &mut Client,
    net_rx: &mut mpsc::UnboundedReceiver<StreamEvent>,
) -> Result<(), TuiError> {
    while !model.quit {
        terminal.draw(|f| ui(f, model))?;

        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(model, client, key).await;
        }

        while let Ok(event) = net_rx.try_recv() {
            model.apply_stream(event);
        }
    }
    Ok(())
}

async fn handle_key(model: &mut TuiModel, client: &mut Client, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Char('q' | 'c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            model.quit = true
        }
        KeyCode::Char('q') => model.quit = true,
        KeyCode::Down | KeyCode::Char('j') => {
            model.selected = (model.selected + 1).min(model.services.len().saturating_sub(1))
        }
        KeyCode::Up | KeyCode::Char('k') => model.selected = model.selected.saturating_sub(1),
        KeyCode::Char('L') => {
            if let Some(row) = model.services.get(model.selected) {
                match client.logs(&row.name, 100).await {
                    Ok(Logs { lines }) => {
                        model.logs.insert(
                            row.name.clone(),
                            lines.into_iter().map(|l| l.line).collect(),
                        );
                    }
                    Err(e) => model.message = Some(format!("logs: {e}")),
                }
            }
        }
        key @ (KeyCode::Char('s') | KeyCode::Char('x') | KeyCode::Char('r')) => {
            let Some(row) = model.services.get(model.selected) else {
                return;
            };
            let name = row.name.clone();
            let verb = if key == KeyCode::Char('s') {
                Verb::ServiceStart {
                    name,
                    wait_for_ready: None,
                }
            } else if key == KeyCode::Char('x') {
                Verb::ServiceStop { name }
            } else {
                Verb::ServiceRestart { name }
            };
            match client.call(&verb).await {
                Reply::Ok(_) => model.message = None,
                Reply::Err { message } => model.message = Some(message),
            }
        }
        _ => {}
    }
}

fn ui(f: &mut ratatui::Frame, model: &TuiModel) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(f.area());
    let [left_main, _bar] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(left);
    let [right_logs, right_msg] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(right);

    let items: Vec<ListItem> = model
        .services
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let selected = i == model.selected;
            let style = Style::default()
                .fg(dot_color(s.state))
                .add_modifier(if selected {
                    Modifier::REVERSED | Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            ListItem::new(Line::styled(
                format!(
                    "{:>14}  {:>3}  {}",
                    status_name(s.state),
                    s.port.map(|p| format!(":{p}")).unwrap_or_default(),
                    s.name,
                ),
                style,
            ))
        })
        .collect();

    f.render_widget(
        List::new(items).block(Block::new().borders(Borders::ALL).title("services")),
        left_main,
    );

    let selected = model.services.get(model.selected).cloned();
    let log_title = selected
        .as_ref()
        .map(|s| format!("logs — {}", s.name))
        .unwrap_or_else(|| "logs".into());
    let log_lines = selected
        .as_ref()
        .and_then(|s| model.logs.get(&s.name))
        .cloned()
        .unwrap_or_else(|| vec!["press L to load tail from the engine".to_string()]);
    let log_text: Vec<Line> = log_lines.iter().map(|l| Line::from(l.clone())).collect();
    f.render_widget(
        Paragraph::new(log_text).block(Block::new().borders(Borders::ALL).title(log_title)),
        right_logs,
    );

    if let Some(message) = &model.message {
        f.render_widget(
            Paragraph::new(message.clone()).style(Style::default().fg(Color::Red)),
            right_msg,
        );
    }

    let bar = Paragraph::new(format!(
        " profile: {}  (q quit, j/k select, s/x/r, L logs)",
        model.profile
    ));
    f.render_widget(bar, _bar);
}

fn dot_color(state: ServiceState) -> Color {
    match state {
        ServiceState::Idle => Color::Gray,
        ServiceState::Starting | ServiceState::Compiling => Color::Yellow,
        ServiceState::Up => Color::Green,
        ServiceState::Failed => Color::Red,
        ServiceState::Stopping => Color::DarkGray,
    }
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn teardown() -> Result<(), TuiError> {
    crossterm::execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    disable_raw_mode()?;
    Ok(())
}

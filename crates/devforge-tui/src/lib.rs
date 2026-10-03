use std::io::{self, Stdout};
use std::sync::Arc;

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

use devforge_core::state::ServiceState;

#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal io: {0}")]
    Io(#[from] io::Error),
    #[error("engine socket: {0}")]
    Engine(String),
}

/// Everything the UI renders; updated from the IPC state stream.
pub struct TuiModel {
    pub profile: String,
    pub services: Vec<ServiceRow>,
    pub selected: usize,
    pub quit: bool,
}

pub struct ServiceRow {
    pub name: String,
    pub port: Option<u16>,
    pub state: ServiceState,
    pub last_line: String,
}

impl Default for TuiModel {
    fn default() -> Self {
        Self {
            profile: "all".into(),
            services: Vec::new(),
            selected: 0,
            quit: false,
        }
    }
}

pub type Tx = mpsc::Sender<devforge_core::ipc::Verb>;

/// Run the TUI against the daemon socket at `socket_path`.
/// `events` carries StreamEvent pushes from the IPC stream task (not yet wired).
pub async fn run(
    _socket_path: &str,
    events: mpsc::UnboundedReceiver<devforge_core::ipc::StreamEvent>,
) -> Result<(), TuiError> {
    let mut terminal = setup()?;
    let _events = events;
    let mut model = TuiModel::default();
    let (tx, mut rx) = mpsc::channel::<devforge_core::ipc::Verb>(16);
    let tx = Arc::new(tx);

    let res = event_loop(&mut terminal, &mut model, tx.clone(), &mut rx).await;

    teardown()?;
    res
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    model: &mut TuiModel,
    _tx: Arc<Tx>,
    _rx: &mut mpsc::Receiver<devforge_core::ipc::Verb>,
) -> Result<(), TuiError> {
    // TODO(M1): connect socket, send ScenarioStatus, subscribe to the state
    // stream, and drive `model` from StreamEvents + Verb replies.
    while !model.quit {
        terminal.draw(|f| ui(f, model))?;

        if event::poll(std::time::Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q' | 'c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    model.quit = true
                }
                KeyCode::Char('q') => model.quit = true,
                KeyCode::Down | KeyCode::Char('j') => {
                    model.selected =
                        (model.selected + 1).min(model.services.len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    model.selected = model.selected.saturating_sub(1)
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn ui(f: &mut ratatui::Frame, model: &TuiModel) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .areas(f.area());
    let [_left_main, _bar] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(left);

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
                    "{:>6}  {}{}  {}",
                    s.state.dot(),
                    s.name,
                    s.port.map(|p| format!(":{p}")).unwrap_or_default(),
                    s.last_line,
                ),
                style,
            ))
        })
        .collect();

    f.render_widget(
        List::new(items).block(Block::new().borders(Borders::ALL).title("services")),
        left,
    );

    let selected = model.services.get(model.selected);
    let log_title = selected
        .map(|s| format!("logs — {}", s.name))
        .unwrap_or_else(|| "logs".into());
    f.render_widget(Block::new().borders(Borders::ALL).title(log_title), right);

    // Note: Paragraph log pane lives here once M1 wires `service_logs` replies
    // into a ring buffer keyed by service name.

    let bar = Paragraph::new(format!(" profile: {}  (q quit, j/k select)", model.profile));
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

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use destilation_core::domain::{Job, Task, ProviderId};
use destilation_core::provider::ProviderConfig;
use destilation_core::metrics::{Metrics, MetricsSnapshot};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{collections::HashMap, io, sync::{mpsc, Arc}, time::Duration};

pub enum TuiCommand {
    PauseJob(JobId),
    ResumeJob(JobId),
    DeleteJob(JobId),
    CleanDatabase,
    SaveProvider(ProviderConfig),
    DeleteProvider(ProviderId),
}

pub enum TuiUpdate {
    Jobs(Vec<Job>),
    Tasks(JobId, Vec<Task>),
    Providers(Vec<ProviderConfig>),
}

type JobId = String;

struct TuiState {
    jobs: Vec<Job>,
    tasks: HashMap<JobId, Vec<Task>>,
    job_list_state: ListState,
    selected_job_id: Option<JobId>,
    view: View,
    show_help: bool,
    is_done: bool,
    
    // Provider Management
    providers: Vec<ProviderConfig>,
    provider_list_state: ListState,
    editing_provider: Option<ProviderConfig>, // For the edit form
    edit_field_index: usize,
    
    // Model Selection State
    model_search_query: String,
    filtered_models: Vec<String>,
    available_models: Vec<String>,
    model_list_state: ListState,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Dashboard,
    JobDetail,
    Settings,
    ModelSelect,
    ProviderList,
    ProviderEdit,
}

impl TuiState {
    fn new() -> Self {
        let mut state = Self {
            jobs: Vec::new(),
            tasks: HashMap::new(),
            job_list_state: ListState::default(),
            selected_job_id: None,
            view: View::Dashboard,
            show_help: false,
            is_done: false,
            
            providers: Vec::new(),
            provider_list_state: ListState::default(),
            editing_provider: None,
            edit_field_index: 0,

            model_search_query: String::new(),
            filtered_models: Vec::new(),
            available_models: vec![
                "openai/gpt-3.5-turbo".to_string(),
                "openai/gpt-4".to_string(),
                "openai/gpt-4-turbo".to_string(),
                "anthropic/claude-3-opus".to_string(),
                "anthropic/claude-3-sonnet".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "google/gemini-pro".to_string(),
                "mistralai/mistral-7b-instruct".to_string(),
                "meta-llama/llama-3-70b-instruct".to_string(),
                "meta-llama/llama-3-8b-instruct".to_string(),
            ],
            model_list_state: ListState::default(),
        };
        state.update_filtered_models();
        state
    }

    fn update_filtered_models(&mut self) {
        if self.model_search_query.is_empty() {
            self.filtered_models = self.available_models.clone();
        } else {
            let query = self.model_search_query.to_lowercase();
            self.filtered_models = self.available_models
                .iter()
                .filter(|m| m.to_lowercase().contains(&query))
                .cloned()
                .collect();
        }
        // Reset selection if list changes
        if !self.filtered_models.is_empty() {
             self.model_list_state.select(Some(0));
        } else {
             self.model_list_state.select(None);
        }
    }

    fn next_job(&mut self) {
        if self.jobs.is_empty() {
            return;
        }
        let i = match self.job_list_state.selected() {
            Some(i) => {
                if i >= self.jobs.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.job_list_state.select(Some(i));
    }

    fn previous_job(&mut self) {
        if self.jobs.is_empty() {
            return;
        }
        let i = match self.job_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.jobs.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.job_list_state.select(Some(i));
    }

    fn enter_job(&mut self) {
        if let Some(i) = self.job_list_state.selected() {
            if let Some(job) = self.jobs.get(i) {
                self.selected_job_id = Some(job.id.clone());
                self.view = View::JobDetail;
            }
        }
    }

    fn exit_detail(&mut self) {
        self.view = View::Dashboard;
        self.selected_job_id = None;
    }
}

pub struct Tui {
    metrics: Arc<dyn Metrics>,
    rx: mpsc::Receiver<TuiUpdate>,
    tx_cmd: mpsc::Sender<TuiCommand>,
    state: TuiState,
}

impl Tui {
    pub fn new(
        metrics: Arc<dyn Metrics>,
        rx: mpsc::Receiver<TuiUpdate>,
        tx_cmd: mpsc::Sender<TuiCommand>,
    ) -> Self {
        Self {
            metrics,
            rx,
            tx_cmd,
            state: TuiState::new(),
        }
    }

    pub fn run(&mut self, check_done: impl Fn() -> bool) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let res = self.run_app(&mut terminal, check_done);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        if let Err(err) = res {
            println!("{:?}", err);
        }

        Ok(())
    }

    fn run_app<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        check_done: impl Fn() -> bool,
    ) -> io::Result<()> {
        loop {
            // Process all pending updates
            while let Ok(update) = self.rx.try_recv() {
                match update {
                    TuiUpdate::Jobs(jobs) => {
                        self.state.jobs = jobs;
                        // Select first job if none selected and jobs exist
                        if self.state.job_list_state.selected().is_none() && !self.state.jobs.is_empty() {
                            self.state.job_list_state.select(Some(0));
                        }
                    }
                    TuiUpdate::Tasks(job_id, tasks) => {
                        self.state.tasks.insert(job_id, tasks);
                    }
                    TuiUpdate::Providers(providers) => {
                        self.state.providers = providers;
                        if self.state.provider_list_state.selected().is_none() && !self.state.providers.is_empty() {
                            self.state.provider_list_state.select(Some(0));
                        }
                    }
                }
            }

            let snap = self.metrics.snapshot();
            terminal
                .draw(|f| ui(f, &snap, &mut self.state))
                .map_err(|_| io::Error::other("draw failed"))?;

            if check_done() {
                self.state.is_done = true;
            }

            let state = &mut self.state;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Mouse(mouse) => {
                        if mouse.kind == event::MouseEventKind::Down(event::MouseButton::Left) {
                             // Minimal mouse support for list selection
                             if state.view == View::ProviderList {
                                 let (_, rows) = crossterm::terminal::size()?;
                                 let list_top = 8; // Margin 1 + Title 3 + Progress 3 + Border 1 approx
                                 if mouse.row >= list_top && mouse.row < rows - 3 {
                                     let idx = (mouse.row - list_top) as usize;
                                     if idx < state.providers.len() {
                                          state.provider_list_state.select(Some(idx));
                                     }
                                 }
                            } else if state.view == View::ProviderEdit {
                                let top_offset = 7; // Margin 1 + Title 3 + Progress 3
                                if mouse.row >= top_offset {
                                    let rel_y = mouse.row - top_offset;
                                    let field_height = 3;
                                    let field_idx = (rel_y / field_height) as usize;
                                    if field_idx < 5 {
                                        state.edit_field_index = field_idx;
                                    }
                                }
                            }
                        }
                    },
                    Event::Key(key) => {
                        // F-Keys global handling
                        match key.code {
                            KeyCode::F(1) => state.show_help = !state.show_help,
                            KeyCode::F(3) => state.view = View::Dashboard, // View
                            KeyCode::F(4) => {
                                // Edit
                                if state.view == View::ProviderList {
                                     if let Some(i) = state.provider_list_state.selected() {
                                         if let Some(p) = state.providers.get(i) {
                                             state.editing_provider = Some(p.clone());
                                             state.view = View::ProviderEdit;
                                             state.edit_field_index = 0;
                                         }
                                     }
                                }
                            },
                            KeyCode::F(7) => {
                                // Create New Provider
                                if state.view == View::ProviderList {
                                    let id = format!("provider-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs());
                                    let new_provider = ProviderConfig::OpenRouter {
                                        id,
                                        name: Some("New Provider".to_string()),
                                        enabled: true,
                                        base_url: "https://openrouter.ai/api/v1".to_string(),
                                        api_key: "".to_string(),
                                        model: "openai/gpt-3.5-turbo".to_string(),
                                    };
                                    state.editing_provider = Some(new_provider);
                                    state.view = View::ProviderEdit;
                                    state.edit_field_index = 0;
                                }
                            },
                            KeyCode::F(8) => {
                                // Delete
                                if state.view == View::ProviderList {
                                    if let Some(i) = state.provider_list_state.selected() {
                                        if let Some(p) = state.providers.get(i) {
                                            let _ = self.tx_cmd.send(TuiCommand::DeleteProvider(p.id().clone()));
                                        }
                                    }
                                }
                            },
                            KeyCode::F(10) => return Ok(()),
                            _ => {}
                        }

                        // Provider Edit specific
                        if state.view == View::ProviderEdit {
                            match key.code {
                                KeyCode::Esc => state.view = View::ProviderList,
                                KeyCode::Tab | KeyCode::Down => state.edit_field_index = (state.edit_field_index + 1) % 5,
                                KeyCode::BackTab | KeyCode::Up => {
                                     if state.edit_field_index == 0 { state.edit_field_index = 4; }
                                     else { state.edit_field_index -= 1; }
                                }
                                KeyCode::Enter | KeyCode::F(2) => {
                                     if let Some(p) = &state.editing_provider {
                                         let _ = self.tx_cmd.send(TuiCommand::SaveProvider(p.clone()));
                                         state.view = View::ProviderList;
                                     }
                                }
                                KeyCode::Char(c) => {
                                    if let Some(p) = &mut state.editing_provider {
                                        update_provider_field(p, state.edit_field_index, c, false);
                                    }
                                }
                                KeyCode::Backspace => {
                                    if let Some(p) = &mut state.editing_provider {
                                        update_provider_field(p, state.edit_field_index, ' ', true);
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }
                        
                        // Handle input for Model Selection
                        if state.view == View::ModelSelect {
                             match key.code {
                                KeyCode::Esc => state.view = View::Settings,
                                KeyCode::Enter => {
                                    // Selection logic (placeholder for now, maybe save to config)
                                    state.view = View::Settings;
                                }
                                KeyCode::Char(c) => {
                                    state.model_search_query.push(c);
                                    state.update_filtered_models();
                                }
                                KeyCode::Backspace => {
                                    state.model_search_query.pop();
                                    state.update_filtered_models();
                                }
                                KeyCode::Down => {
                                    let i = match state.model_list_state.selected() {
                                        Some(i) => {
                                            if i >= state.filtered_models.len().saturating_sub(1) {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.model_list_state.select(Some(i));
                                }
                                KeyCode::Up => {
                                    let i = match state.model_list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                state.filtered_models.len().saturating_sub(1)
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.model_list_state.select(Some(i));
                                }
                                _ => {}
                            }
                            continue;
                        }
                        
                        // Handle Provider List Navigation
                        if state.view == View::ProviderList {
                            match key.code {
                                KeyCode::Esc => state.view = View::Settings,
                                KeyCode::Down => {
                                     let i = match state.provider_list_state.selected() {
                                        Some(i) => {
                                            if i >= state.providers.len().saturating_sub(1) {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.provider_list_state.select(Some(i));
                                }
                                KeyCode::Up => {
                                    let i = match state.provider_list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                state.providers.len().saturating_sub(1)
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.provider_list_state.select(Some(i));
                                }
                                KeyCode::Enter => {
                                     if let Some(i) = state.provider_list_state.selected() {
                                         if let Some(p) = state.providers.get(i) {
                                             state.editing_provider = Some(p.clone());
                                             state.view = View::ProviderEdit;
                                             state.edit_field_index = 0;
                                         }
                                     }
                                }
                                KeyCode::Char(' ') => {
                                    if let Some(i) = state.provider_list_state.selected() {
                                        if let Some(p) = state.providers.get_mut(i) {
                                            // Toggle enabled status
                                            match p {
                                                ProviderConfig::OpenRouter { enabled, .. } => *enabled = !*enabled,
                                                ProviderConfig::Ollama { enabled, .. } => *enabled = !*enabled,
                                                ProviderConfig::Script { enabled, .. } => *enabled = !*enabled,
                                            }
                                            // Save immediately
                                            let _ = self.tx_cmd.send(TuiCommand::SaveProvider(p.clone()));
                                        }
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('h') => state.show_help = !state.show_help,
                            KeyCode::Char('s') => state.view = View::Settings,
                            KeyCode::Char('d') => {
                                if let Some(job_id) = &state.selected_job_id {
                                    let _ = self.tx_cmd.send(TuiCommand::DeleteJob(job_id.clone()));
                                    state.view = View::Dashboard;
                                    state.selected_job_id = None;
                                }
                            }
                            KeyCode::Char('p') => {
                                 if let Some(job_id) = &state.selected_job_id {
                                    // Find job to check status
                                    if let Some(job) = state.jobs.iter().find(|j| &j.id == job_id) {
                                        match job.status {
                                            destilation_core::domain::JobStatus::Running => {
                                                 let _ = self.tx_cmd.send(TuiCommand::PauseJob(job_id.clone()));
                                            },
                                            destilation_core::domain::JobStatus::Paused => {
                                                 let _ = self.tx_cmd.send(TuiCommand::ResumeJob(job_id.clone()));
                                            },
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            KeyCode::Char('c') if state.view == View::Settings => {
                                let _ = self.tx_cmd.send(TuiCommand::CleanDatabase);
                            }
                            KeyCode::Char('o') if state.view == View::Settings => {
                                state.view = View::ModelSelect;
                                state.model_search_query.clear();
                                state.update_filtered_models();
                            }
                            KeyCode::Down => state.next_job(),
                            KeyCode::Up => state.previous_job(),
                            KeyCode::Enter => state.enter_job(),
                            KeyCode::Esc => state.exit_detail(),
                            _ => {}
                        }
                    },
                    _ => {}
                }
        }
    }
}
}

fn ui(f: &mut Frame, snap: &MetricsSnapshot, state: &mut TuiState) {
    // Background
    let size = f.area();
    f.render_widget(Block::default().style(Style::default().bg(Color::Blue)), size);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3), // Title
                Constraint::Length(3), // Global Progress
                Constraint::Min(10),   // Main Content (Dashboard or Detail)
                Constraint::Length(3), // Footer/Info
            ]
            .as_ref(),
        )
        .split(f.area());

    // Title
    let title_text = if state.is_done {
        "Destilation Agentic Pipeline [DONE]"
    } else {
        "Destilation Agentic Pipeline"
    };
    let title_style = if state.is_done {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    };

    let title = Paragraph::new(title_text)
        .style(title_style)
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(title, chunks[0]);

    // Global Progress
    let ratio = if snap.tasks_enqueued > 0 {
        (snap.tasks_persisted as f64 / snap.tasks_enqueued as f64).min(1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .block(Block::default().title("Total Progress").borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(ratio);
    f.render_widget(gauge, chunks[1]);

    // Main Content
    match state.view {
        View::Dashboard => render_dashboard(f, chunks[2], snap, state),
        View::JobDetail => render_job_detail(f, chunks[2], state),
        View::Settings => render_settings(f, chunks[2]),
        View::ModelSelect => render_model_select(f, chunks[2], state),
        View::ProviderList => render_provider_list(f, chunks[2], state),
        View::ProviderEdit => render_provider_edit(f, chunks[2], state),
    }

    // Footer with F-keys
    let f_keys = vec![
        "F1 Help", "F2 Save", "F3 View", "F4 Edit", "F5 Copy", "F6 Move", "F7 MkDir", "F8 Delete", "F10 Quit"
    ];
    let f_key_spans: Vec<Span> = f_keys.iter().enumerate().map(|(i, k)| {
        if i > 0 {
             Span::raw(format!(" {} ", k))
        } else {
             Span::raw(format!("{}", k))
        }
    }).collect();
    
    let footer = Paragraph::new(Line::from(f_key_spans))
        .style(Style::default().bg(Color::Blue).fg(Color::White))
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(footer, chunks[3]);
}

fn render_provider_list(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let items: Vec<ListItem> = state.providers.iter().map(|p| {
        let name = p.name().cloned().unwrap_or_else(|| {
             match p {
                 ProviderConfig::OpenRouter { model, .. } => model.clone(),
                 ProviderConfig::Ollama { model, .. } => model.clone(),
                 ProviderConfig::Script { command, .. } => command.clone(),
             }
        });
        let id = p.id();
        let status = if p.is_enabled() { "[x]" } else { "[ ]" };
        ListItem::new(format!("{} {} ({})", status, name, id))
    }).collect();

    let list = List::new(items)
        .block(Block::default().title("Providers").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    
    f.render_stateful_widget(list, area, &mut state.provider_list_state);
}

fn render_provider_edit(f: &mut Frame, area: Rect, state: &mut TuiState) {
    if let Some(provider) = &state.editing_provider {
        let (name, base_url, api_key, model, enabled) = match provider {
            ProviderConfig::OpenRouter { name, base_url, api_key, model, enabled, .. } => 
                (name.clone().unwrap_or_default(), base_url.clone(), api_key.clone(), model.clone(), *enabled),
            ProviderConfig::Ollama { name, base_url, model, enabled, .. } =>
                (name.clone().unwrap_or_default(), base_url.clone(), "N/A".to_string(), model.clone(), *enabled),
            ProviderConfig::Script { name, command, enabled, .. } =>
                (name.clone().unwrap_or_default(), command.clone(), "N/A".to_string(), "N/A".to_string(), *enabled),
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Name
                Constraint::Length(3), // Base URL / Command
                Constraint::Length(3), // API Key
                Constraint::Length(3), // Model
                Constraint::Length(3), // Enabled
            ].as_ref())
            .split(area);

        let fields = vec![
            ("Name", name),
            ("Base URL / Command", base_url),
            ("API Key", api_key),
            ("Model", model),
            ("Enabled", enabled.to_string()),
        ];

        for (i, (label, value)) in fields.iter().enumerate() {
            if i >= chunks.len() { break; }
            let style = if state.edit_field_index == i {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let p = Paragraph::new(format!("{}", value))
                .block(Block::default().title(*label).borders(Borders::ALL).border_style(style));
            f.render_widget(p, chunks[i]);
        }
    }
}

fn render_settings(f: &mut Frame, area: Rect) {
    let text = vec![
        Line::from("Settings"),
        Line::from(""),
        Line::from(vec![
            Span::raw("Database Cleaning: "),
            Span::styled("[c] Clean Database", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw(" (Wipes all jobs and tasks)"),
        ]),
        Line::from(""),
        Line::from(vec![
             Span::raw("Provider Config: "),
             Span::styled("[o] OpenRouter Model", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        ]),
    ];
    let block = Paragraph::new(text)
        .block(Block::default().title("Settings").borders(Borders::ALL));
    f.render_widget(block, area);
}

fn render_model_select(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3), // Search Bar
                Constraint::Min(5),    // List
            ]
            .as_ref(),
        )
        .split(area);

    // Search Bar
    let search_text = format!("Search: {}", state.model_search_query);
    let search_block = Paragraph::new(search_text)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("Type to Filter"));
    f.render_widget(search_block, chunks[0]);

    // Model List
    let items: Vec<ListItem> = state
        .filtered_models
        .iter()
        .map(|m| {
            ListItem::new(Line::from(vec![
                Span::raw(m),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title("Available Models").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    f.render_stateful_widget(list, chunks[1], &mut state.model_list_state);
}

fn render_dashboard(f: &mut Frame, area: Rect, snap: &MetricsSnapshot, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(area);

    // Left: Metrics Summary
    let stats_text = vec![
        Line::from(vec![
            Span::raw("Jobs: "),
            Span::styled(
                format!("{}", snap.jobs_submitted),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(" | Tasks: "),
            Span::styled(
                format!("{}", snap.tasks_enqueued),
                Style::default().fg(Color::Blue),
            ),
        ]),
        Line::from(vec![
            Span::raw("Started: "),
            Span::styled(
                format!("{}", snap.tasks_started),
                Style::default().fg(Color::Magenta),
            ),
        ]),
        Line::from(vec![
            Span::raw("Persisted: "),
            Span::styled(
                format!("{}", snap.tasks_persisted),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::raw("Rejected: "),
            Span::styled(
                format!("{}", snap.tasks_rejected),
                Style::default().fg(Color::Red),
            ),
        ]),
    ];
    let stats =
        Paragraph::new(stats_text).block(Block::default().title("Global Metrics").borders(Borders::ALL));
    f.render_widget(stats, chunks[0]);

    // Right: Job List
    let items: Vec<ListItem> = state
        .jobs
        .iter()
        .map(|job| {
            let status_style = match job.status {
                destilation_core::domain::JobStatus::Completed => Style::default().fg(Color::Green),
                destilation_core::domain::JobStatus::Failed => Style::default().fg(Color::Red),
                destilation_core::domain::JobStatus::Running => Style::default().fg(Color::Yellow),
                _ => Style::default(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", job.id), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:?}", job.status), status_style),
                Span::raw(format!(" - {}", job.config.name)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title("Active Jobs").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, chunks[1], &mut state.job_list_state);
}

fn render_job_detail(f: &mut Frame, area: Rect, state: &TuiState) {
    let job_id = match &state.selected_job_id {
        Some(id) => id,
        None => return,
    };
    
    let job = match state.jobs.iter().find(|j| &j.id == job_id) {
        Some(j) => j,
        None => return,
    };

    let tasks = state.tasks.get(job_id).map(|v| v.as_slice()).unwrap_or(&[]);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(5)].as_ref())
        .split(area);

    // Job Header
    let header_text = vec![
        Line::from(vec![Span::raw("ID: "), Span::styled(&job.id, Style::default().fg(Color::Cyan))]),
        Line::from(vec![Span::raw("Name: "), Span::raw(&job.config.name)]),
        Line::from(vec![Span::raw("Status: "), Span::raw(format!("{:?}", job.status))]),
        Line::from(vec![Span::raw("Tasks: "), Span::raw(format!("{}", tasks.len()))]),
    ];
    let header = Paragraph::new(header_text).block(Block::default().title("Job Details").borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    // Task List
    let items: Vec<ListItem> = tasks
        .iter()
        .map(|task| {
            let state_style = match task.state {
                destilation_core::domain::TaskState::Persisted => Style::default().fg(Color::Green),
                destilation_core::domain::TaskState::Rejected => Style::default().fg(Color::Red),
                destilation_core::domain::TaskState::Generating => Style::default().fg(Color::Yellow),
                destilation_core::domain::TaskState::Validating => Style::default().fg(Color::Magenta),
                _ => Style::default(),
            };
            
            ListItem::new(Line::from(vec![
                Span::raw(format!("Task {} ", task.id)),
                Span::styled(format!("{:?}", task.state), state_style),
                Span::raw(format!(" (Att: {})", task.attempts)),
            ]))
        })
        .collect();
    
    let list = List::new(items)
        .block(Block::default().title("Tasks").borders(Borders::ALL));
    f.render_widget(list, chunks[1]);
}

fn update_provider_field(config: &mut ProviderConfig, index: usize, c: char, is_backspace: bool) {
    match config {
        ProviderConfig::OpenRouter { name, base_url, api_key, model, enabled, .. } => {
            match index {
                0 => modify_string_opt(name, c, is_backspace),
                1 => modify_string(base_url, c, is_backspace),
                2 => modify_string(api_key, c, is_backspace),
                3 => modify_string(model, c, is_backspace),
                4 => if !is_backspace && c == ' ' { *enabled = !*enabled; }, 
                _ => {}
            }
        },
        ProviderConfig::Ollama { name, base_url, model, enabled, .. } => {
             match index {
                0 => modify_string_opt(name, c, is_backspace),
                1 => modify_string(base_url, c, is_backspace),
                2 => {}, 
                3 => modify_string(model, c, is_backspace),
                4 => if !is_backspace && c == ' ' { *enabled = !*enabled; },
                _ => {}
            }
        },
        ProviderConfig::Script { name, command, enabled, .. } => {
             match index {
                0 => modify_string_opt(name, c, is_backspace),
                1 => modify_string(command, c, is_backspace),
                4 => if !is_backspace && c == ' ' { *enabled = !*enabled; },
                _ => {}
            }
        }
    }
}

fn modify_string(s: &mut String, c: char, is_backspace: bool) {
    if is_backspace {
        s.pop();
    } else {
        s.push(c);
    }
}

fn modify_string_opt(s: &mut Option<String>, c: char, is_backspace: bool) {
    if s.is_none() {
        *s = Some(String::new());
    }
    if let Some(val) = s {
        modify_string(val, c, is_backspace);
    }
}

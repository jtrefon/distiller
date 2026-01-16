use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use destilation_core::domain::{
    Job, JobConfig, ProviderId, Task, TemplateConfig,
};
use destilation_core::metrics::{Metrics, MetricsSnapshot};
use destilation_core::provider::ProviderConfig;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{
    collections::HashMap,
    io,
    sync::{mpsc, Arc},
    time::Duration,
};

pub enum TuiCommand {
    StartJob(JobConfig),
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
    Templates(Vec<TemplateConfig>),
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

    // Job Creation State
    templates: Vec<TemplateConfig>,
    job_create_name: String,
    job_create_domain: String,
    job_create_template_state: ListState,
    job_create_field_idx: usize,

    // Provider Creation State
    provider_type_idx: usize,

    // Global Navigation State
    focus: FocusArea,
    selected_f_key_index: usize,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Dashboard,
    JobDetail,
    JobCreate,
    Settings,
    ModelSelect,
    ProviderList,
    ProviderEdit,
    ProviderTypeSelect,
}

#[derive(PartialEq, Clone, Copy)]
enum FocusArea {
    Main,
    BottomBar,
}

impl TuiState {
    fn new() -> Self {
        let state = Self {
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
                "openai/gpt-4o".to_string(),
                "openai/gpt-4o-mini".to_string(),
                "openai/gpt-4-turbo".to_string(),
                "openai/gpt-3.5-turbo".to_string(),
                "anthropic/claude-3.5-sonnet".to_string(),
                "anthropic/claude-3-opus".to_string(),
                "anthropic/claude-3-sonnet".to_string(),
                "anthropic/claude-3-haiku".to_string(),
                "google/gemini-flash-1.5".to_string(),
                "google/gemini-pro-1.5".to_string(),
                "google/gemini-pro".to_string(),
                "meta-llama/llama-3-70b-instruct".to_string(),
                "meta-llama/llama-3-8b-instruct".to_string(),
                "mistralai/mixtral-8x22b-instruct".to_string(),
                "mistralai/mixtral-8x7b-instruct".to_string(),
                "mistralai/mistral-7b-instruct".to_string(),
                "mistralai/mistral-7b-instruct:free".to_string(),
                "openchat/openchat-7b:free".to_string(),
                "gryphe/mythomist-7b:free".to_string(),
                "nousresearch/nous-capybara-7b:free".to_string(),
            ],
            model_list_state: ListState::default(),

            templates: Vec::new(),
            job_create_name: String::new(),
            job_create_domain: String::new(),
            job_create_template_state: ListState::default(),
            job_create_field_idx: 0,

            provider_type_idx: 0,

            focus: FocusArea::Main,
            selected_f_key_index: 0,
        };
        state
    }

    fn update_filtered_models(&mut self) {
        if self.model_search_query.is_empty() {
            self.filtered_models = self.available_models.clone();
        } else {
            let query = self.model_search_query.to_lowercase();
            self.filtered_models = self
                .available_models
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
                        if self.state.job_list_state.selected().is_none()
                            && !self.state.jobs.is_empty()
                        {
                            self.state.job_list_state.select(Some(0));
                        }
                    }
                    TuiUpdate::Tasks(job_id, tasks) => {
                        self.state.tasks.insert(job_id, tasks);
                    }
                    TuiUpdate::Providers(providers) => {
                        self.state.providers = providers;
                        if self.state.provider_list_state.selected().is_none()
                            && !self.state.providers.is_empty()
                        {
                            self.state.provider_list_state.select(Some(0));
                        }
                    }
                    TuiUpdate::Templates(templates) => {
                        self.state.templates = templates;
                        if !self.state.templates.is_empty() {
                            self.state.job_create_template_state.select(Some(0));
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
                            let (cols, rows) = crossterm::terminal::size()?;
                            // Footer Mouse Handling
                            if mouse.row >= rows - 4 && mouse.row <= rows - 2 {
                                let f_keys = get_f_keys(state.view);
                                let visible_f_keys: Vec<(usize, &str, &str)> = f_keys
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, (_, label))| !label.is_empty())
                                    .map(|(i, (key, label))| (i, *key, *label))
                                    .collect();

                                let visible_count = visible_f_keys.len();
                                if visible_count > 0 {
                                    let btn_width = (cols as f64 - 2.0) / visible_count as f64;
                                    let col = (mouse.column as f64 - 1.0).max(0.0);
                                    let clicked_idx = (col / btn_width).floor() as usize;
                                    if clicked_idx < visible_count {
                                        let original_idx = visible_f_keys[clicked_idx].0;
                                        state.selected_f_key_index = original_idx;
                                        state.focus = FocusArea::BottomBar;

                                        // Execute action
                                        match state.selected_f_key_index {
                                            0 => state.show_help = !state.show_help,
                                            1 => {
                                                if state.view == View::ProviderEdit {
                                                    if let Some(p) = &state.editing_provider {
                                                        let _ = self.tx_cmd.send(
                                                            TuiCommand::SaveProvider(p.clone()),
                                                        );
                                                        state.view = View::ProviderList;
                                                        state.focus = FocusArea::Main;
                                                    }
                                                } else if state.view == View::JobCreate {
                                                    if !state.job_create_name.is_empty() && !state.job_create_domain.is_empty() {
                                                        if let Some(idx) = state.job_create_template_state.selected() {
                                                            if let Some(template) = state.templates.get(idx) {
                                                                let providers: Vec<destilation_core::domain::JobProviderSpec> = state.providers.iter()
                                                                    .filter(|p| p.is_enabled())
                                                                    .map(|p| destilation_core::domain::JobProviderSpec {
                                                                        provider_id: p.id().clone(),
                                                                        weight: 1.0,
                                                                        capabilities_required: vec![],
                                                                    })
                                                                    .collect();

                                                                let domains = vec![destilation_core::domain::DomainSpec {
                                                                    id: format!("domain-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                                    name: state.job_create_domain.clone(),
                                                                    weight: 1.0,
                                                                    tags: vec![],
                                                                }];

                                                                let config = JobConfig {
                                                                    id: format!("job-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                                    name: state.job_create_name.clone(),
                                                                    description: None,
                                                                    target_samples: 100,
                                                                    max_concurrency: 4,
                                                                    domains,
                                                                    template_id: template.id.clone(),
                                                                    reasoning_mode: destilation_core::domain::ReasoningMode::Simple,
                                                                    providers,
                                                                    validation: destilation_core::domain::JobValidationConfig {
                                                                        max_attempts: 3,
                                                                        validators: vec![],
                                                                        min_quality_score: None,
                                                                        fail_fast: false,
                                                                    },
                                                                    output: destilation_core::domain::JobOutputConfig {
                                                                        dataset_dir: "./datasets".to_string(),
                                                                        shard_size: 1000,
                                                                        compress: false,
                                                                        metadata: HashMap::new(),
                                                                    },
                                                                };
                                                                let _ = self.tx_cmd.send(TuiCommand::StartJob(config));
                                                                state.view = View::Dashboard;
                                                            }
                                                        }
                                                    }
                                                } else if state.view == View::Dashboard {
                                                    if let Some(job_id) = &state.selected_job_id {
                                                        if let Some(job) = state.jobs.iter().find(|j| &j.id == job_id) {
                                                            match job.status {
                                                                destilation_core::domain::JobStatus::Running => {
                                                                    let _ = self.tx_cmd.send(TuiCommand::PauseJob(job_id.clone()));
                                                                }
                                                                destilation_core::domain::JobStatus::Paused | destilation_core::domain::JobStatus::Pending => {
                                                                    let _ = self.tx_cmd.send(TuiCommand::ResumeJob(job_id.clone()));
                                                                }
                                                                _ => {}
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            2 => {
                                                state.view = View::Dashboard;
                                                state.focus = FocusArea::Main;
                                            }
                                            3 => {
                                                if state.view == View::ProviderList {
                                                    if let Some(i) =
                                                        state.provider_list_state.selected()
                                                    {
                                                        if let Some(p) = state.providers.get(i) {
                                                            state.editing_provider =
                                                                Some(p.clone());
                                                            state.view = View::ProviderEdit;
                                                            state.edit_field_index = 0;
                                                            state.focus = FocusArea::Main;
                                                        }
                                                    }
                                                }
                                            }
                                            6 => {
                                                if state.view == View::ProviderList {
                                                    state.view = View::ProviderTypeSelect;
                                                    state.provider_type_idx = 0;
                                                    state.focus = FocusArea::Main;
                                                }
                                            }
                                            7 => {
                                                if state.view == View::ProviderList {
                                                    if let Some(i) =
                                                        state.provider_list_state.selected()
                                                    {
                                                        if let Some(p) = state.providers.get(i) {
                                                            let _ = self.tx_cmd.send(
                                                                TuiCommand::DeleteProvider(
                                                                    p.id().clone(),
                                                                ),
                                                            );
                                                        }
                                                    }
                                                } else if state.view == View::Dashboard {
                                                    if let Some(job_id) = &state.selected_job_id {
                                                        let _ = self.tx_cmd.send(
                                                            TuiCommand::DeleteJob(job_id.clone()),
                                                        );
                                                        state.selected_job_id = None;
                                                    }
                                                }
                                            }
                                            8 => return Ok(()),
                                            9 => {
                                                state.view = View::Settings;
                                                state.focus = FocusArea::Main;
                                            }
                                            10 => {
                                                state.view = View::ProviderList;
                                                state.focus = FocusArea::Main;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            } else if state.view == View::ProviderList {
                                let list_top = 8;
                                if mouse.row >= list_top && mouse.row < rows - 3 {
                                    let idx = (mouse.row - list_top) as usize;
                                    if idx < state.providers.len() {
                                        state.provider_list_state.select(Some(idx));
                                    }
                                }
                            } else if state.view == View::ProviderEdit {
                                let top_offset = 4;
                                if mouse.row >= top_offset {
                                    let rel_y = mouse.row - top_offset;
                                    let field_height = 3;
                                    let field_idx = (rel_y / field_height) as usize;
                                    if field_idx < 5 {
                                        state.edit_field_index = field_idx;
                                        if field_idx == 3 {
                                            if let Some(p) = &state.editing_provider {
                                                if !matches!(p, ProviderConfig::Script { .. }) {
                                                    state.view = View::ModelSelect;
                                                    state.model_search_query.clear();
                                                    state.update_filtered_models();
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Event::Key(key) => {
                        if state.show_help {
                            if key.code == KeyCode::Esc || key.code == KeyCode::F(1) {
                                state.show_help = false;
                            }
                            continue;
                        }

                        match key.code {
                            KeyCode::F(1) => state.show_help = !state.show_help,
                            KeyCode::F(2) => {
                                if state.view == View::ProviderEdit {
                                    if let Some(p) = &state.editing_provider {
                                        let _ =
                                            self.tx_cmd.send(TuiCommand::SaveProvider(p.clone()));
                                        state.view = View::ProviderList;
                                        state.focus = FocusArea::Main;
                                    }
                                } else if state.view == View::JobCreate {
                                    if !state.job_create_name.is_empty() && !state.job_create_domain.is_empty() {
                                        if let Some(idx) = state.job_create_template_state.selected() {
                                            if let Some(template) = state.templates.get(idx) {
                                                let providers: Vec<destilation_core::domain::JobProviderSpec> = state.providers.iter()
                                                    .filter(|p| p.is_enabled())
                                                    .map(|p| destilation_core::domain::JobProviderSpec {
                                                        provider_id: p.id().clone(),
                                                        weight: 1.0,
                                                        capabilities_required: vec![],
                                                    })
                                                    .collect();

                                                let domains = vec![destilation_core::domain::DomainSpec {
                                                    id: format!("domain-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                    name: state.job_create_domain.clone(),
                                                    weight: 1.0,
                                                    tags: vec![],
                                                }];

                                                let config = JobConfig {
                                                    id: format!("job-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                    name: state.job_create_name.clone(),
                                                    description: None,
                                                    target_samples: 100,
                                                    max_concurrency: 4,
                                                    domains,
                                                    template_id: template.id.clone(),
                                                    reasoning_mode: destilation_core::domain::ReasoningMode::Simple,
                                                    providers,
                                                    validation: destilation_core::domain::JobValidationConfig {
                                                        max_attempts: 3,
                                                        validators: vec![],
                                                        min_quality_score: None,
                                                        fail_fast: false,
                                                    },
                                                    output: destilation_core::domain::JobOutputConfig {
                                                        dataset_dir: "./datasets".to_string(),
                                                        shard_size: 1000,
                                                        compress: false,
                                                        metadata: HashMap::new(),
                                                    },
                                                };
                                                let _ = self.tx_cmd.send(TuiCommand::StartJob(config));
                                                state.view = View::Dashboard;
                                            }
                                        }
                                    }
                                } else if state.view == View::Dashboard {
                                    if let Some(job_id) = &state.selected_job_id {
                                        if let Some(job) = state.jobs.iter().find(|j| &j.id == job_id) {
                                            match job.status {
                                                destilation_core::domain::JobStatus::Running => {
                                                    let _ = self.tx_cmd.send(TuiCommand::PauseJob(job_id.clone()));
                                                }
                                                destilation_core::domain::JobStatus::Paused | destilation_core::domain::JobStatus::Pending => {
                                                    let _ = self.tx_cmd.send(TuiCommand::ResumeJob(job_id.clone()));
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            KeyCode::F(3) => {
                                state.view = View::Dashboard;
                                state.focus = FocusArea::Main;
                            }
                            KeyCode::F(4) => {
                                if state.view == View::ProviderList {
                                    if let Some(i) = state.provider_list_state.selected() {
                                        if let Some(p) = state.providers.get(i) {
                                            state.editing_provider = Some(p.clone());
                                            state.view = View::ProviderEdit;
                                            state.edit_field_index = 0;
                                            state.focus = FocusArea::Main;
                                        }
                                    }
                                }
                            }
                            KeyCode::F(7) => {
                                if state.view == View::ProviderList {
                                    state.view = View::ProviderTypeSelect;
                                    state.provider_type_idx = 0;
                                    state.focus = FocusArea::Main;
                                } else if state.view == View::Dashboard {
                                    state.view = View::JobCreate;
                                    state.job_create_name = String::new();
                                    state.job_create_field_idx = 0;
                                    state.focus = FocusArea::Main;
                                    if !state.templates.is_empty() {
                                        state.job_create_template_state.select(Some(0));
                                    }
                                }
                            }
                            KeyCode::F(8) => {
                                if state.view == View::ProviderList {
                                    if let Some(i) = state.provider_list_state.selected() {
                                        if let Some(p) = state.providers.get(i) {
                                            let _ = self
                                                .tx_cmd
                                                .send(TuiCommand::DeleteProvider(p.id().clone()));
                                        }
                                    }
                                } else if state.view == View::Dashboard {
                                    if let Some(job_id) = &state.selected_job_id {
                                        let _ =
                                            self.tx_cmd.send(TuiCommand::DeleteJob(job_id.clone()));
                                        state.selected_job_id = None;
                                    }
                                }
                            }
                            KeyCode::F(9) => {
                                if state.view == View::ProviderEdit {
                                    // Check if we are editing a new provider (not in the list yet)
                                    let is_new = if let Some(p) = &state.editing_provider {
                                        !state.providers.iter().any(|existing| existing.id() == p.id())
                                    } else {
                                        false
                                    };
                                    
                                    if is_new {
                                        state.view = View::ProviderTypeSelect;
                                    } else {
                                        state.view = View::ProviderList;
                                    }
                                } else if state.view == View::ModelSelect {
                                    state.view = View::ProviderEdit;
                                }
                            }
                            KeyCode::F(10) => return Ok(()),
                            KeyCode::F(11) => {
                                state.view = View::Settings;
                                state.focus = FocusArea::Main;
                            }
                            KeyCode::F(12) => {
                                state.view = View::ProviderList;
                                state.focus = FocusArea::Main;
                            }
                            _ => {}
                        }

                        if state.focus == FocusArea::BottomBar {
                            let active_indices: Vec<usize> = match state.view {
                                View::Dashboard => vec![0, 1, 2, 6, 7, 9, 10, 11],
                                View::ProviderList => vec![0, 3, 6, 7, 9, 10],
                                View::ProviderEdit => vec![0, 1, 8, 9, 10],
                                View::ModelSelect => vec![0, 8, 9],
                                _ => vec![0, 2, 9, 10, 11],
                            };

                            let current_pos = active_indices
                                .iter()
                                .position(|&x| x == state.selected_f_key_index)
                                .unwrap_or(0);

                            match key.code {
                                KeyCode::Left => {
                                    let new_pos = if current_pos > 0 {
                                        current_pos - 1
                                    } else {
                                        active_indices.len() - 1
                                    };
                                    state.selected_f_key_index = active_indices[new_pos];
                                }
                                KeyCode::Right => {
                                    let new_pos = if current_pos < active_indices.len() - 1 {
                                        current_pos + 1
                                    } else {
                                        0
                                    };
                                    state.selected_f_key_index = active_indices[new_pos];
                                }
                                KeyCode::Esc | KeyCode::Up => state.focus = FocusArea::Main,
                                KeyCode::BackTab => {
                                    if current_pos > 0 {
                                        state.selected_f_key_index =
                                            active_indices[current_pos - 1];
                                    } else {
                                        state.focus = FocusArea::Main;
                                    }
                                }
                                KeyCode::Tab => {
                                    if current_pos < active_indices.len() - 1 {
                                        state.selected_f_key_index =
                                            active_indices[current_pos + 1];
                                    } else {
                                        state.focus = FocusArea::Main;
                                    }
                                }
                                KeyCode::Enter => match state.selected_f_key_index {
                                    0 => state.show_help = !state.show_help,
                                    1 => {
                                        if state.view == View::ProviderEdit {
                                            if let Some(p) = &state.editing_provider {
                                                let _ = self
                                                    .tx_cmd
                                                    .send(TuiCommand::SaveProvider(p.clone()));
                                                state.view = View::ProviderList;
                                                state.focus = FocusArea::Main;
                                            }
                                        } else if state.view == View::JobCreate {
                                            if !state.job_create_name.is_empty() && !state.job_create_domain.is_empty() {
                                                if let Some(idx) = state.job_create_template_state.selected() {
                                                    if let Some(template) = state.templates.get(idx) {
                                                        let providers: Vec<destilation_core::domain::JobProviderSpec> = state.providers.iter()
                                                            .filter(|p| p.is_enabled())
                                                            .map(|p| destilation_core::domain::JobProviderSpec {
                                                                provider_id: p.id().clone(),
                                                                weight: 1.0,
                                                                capabilities_required: vec![],
                                                            })
                                                            .collect();

                                                        let domains = vec![destilation_core::domain::DomainSpec {
                                                            id: format!("domain-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                            name: state.job_create_domain.clone(),
                                                            weight: 1.0,
                                                            tags: vec![],
                                                        }];

                                                        let config = JobConfig {
                                                            id: format!("job-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()),
                                                            name: state.job_create_name.clone(),
                                                            description: None,
                                                            target_samples: 100,
                                                            max_concurrency: 4,
                                                            domains,
                                                            template_id: template.id.clone(),
                                                            reasoning_mode: destilation_core::domain::ReasoningMode::Simple,
                                                            providers,
                                                            validation: destilation_core::domain::JobValidationConfig {
                                                                max_attempts: 3,
                                                                validators: vec![],
                                                                min_quality_score: None,
                                                                fail_fast: false,
                                                            },
                                                            output: destilation_core::domain::JobOutputConfig {
                                                                dataset_dir: "./datasets".to_string(),
                                                                shard_size: 1000,
                                                                compress: false,
                                                                metadata: HashMap::new(),
                                                            },
                                                        };
                                                        let _ = self.tx_cmd.send(TuiCommand::StartJob(config));
                                                        state.view = View::Dashboard;
                                                    }
                                                }
                                            }
                                        } else if state.view == View::Dashboard {
                                            if let Some(job_id) = &state.selected_job_id {
                                                if let Some(job) = state.jobs.iter().find(|j| &j.id == job_id) {
                                                    match job.status {
                                                        destilation_core::domain::JobStatus::Running => {
                                                            let _ = self.tx_cmd.send(TuiCommand::PauseJob(job_id.clone()));
                                                        }
                                                        destilation_core::domain::JobStatus::Paused | destilation_core::domain::JobStatus::Pending => {
                                                            let _ = self.tx_cmd.send(TuiCommand::ResumeJob(job_id.clone()));
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    2 => {
                                        state.view = View::Dashboard;
                                        state.focus = FocusArea::Main;
                                    }
                                    3 => {
                                        if state.view == View::ProviderList {
                                            if let Some(i) = state.provider_list_state.selected() {
                                                if let Some(p) = state.providers.get(i) {
                                                    state.editing_provider = Some(p.clone());
                                                    state.view = View::ProviderEdit;
                                                    state.edit_field_index = 0;
                                                    state.focus = FocusArea::Main;
                                                }
                                            }
                                        }
                                    }
                                    6 => {
                                        if state.view == View::ProviderList {
                                            state.view = View::ProviderTypeSelect;
                                            state.provider_type_idx = 0;
                                            state.focus = FocusArea::Main;
                                        } else if state.view == View::Dashboard {
                                            state.view = View::JobCreate;
                                            state.job_create_name = String::new();
                                            state.job_create_field_idx = 0;
                                            state.focus = FocusArea::Main;
                                            if !state.templates.is_empty() {
                                                state.job_create_template_state.select(Some(0));
                                            }
                                        }
                                    }
                                    7 => {
                                        if state.view == View::ProviderList {
                                            if let Some(i) = state.provider_list_state.selected() {
                                                if let Some(p) = state.providers.get(i) {
                                                    let _ = self.tx_cmd.send(
                                                        TuiCommand::DeleteProvider(p.id().clone()),
                                                    );
                                                }
                                            }
                                        } else if state.view == View::Dashboard {
                                            if let Some(job_id) = &state.selected_job_id {
                                                let _ = self
                                                    .tx_cmd
                                                    .send(TuiCommand::DeleteJob(job_id.clone()));
                                                state.selected_job_id = None;
                                            }
                                        }
                                    }
                                    8 => {
                                        // F9 Back
                                        if state.view == View::ProviderEdit {
                                            let is_new = if let Some(p) = &state.editing_provider {
                                                !state.providers.iter().any(|existing| existing.id() == p.id())
                                            } else {
                                                false
                                            };
                                            
                                            if is_new {
                                                state.view = View::ProviderTypeSelect;
                                            } else {
                                                state.view = View::ProviderList;
                                            }
                                        } else if state.view == View::ModelSelect {
                                            state.view = View::ProviderEdit;
                                        }
                                    }
                                    9 => return Ok(()), // F10 Quit
                                    10 => {
                                        // F11 Settings
                                        state.view = View::Settings;
                                        state.focus = FocusArea::Main;
                                    }
                                    11 => {
                                        // F12 Prov
                                        state.view = View::ProviderList;
                                        state.focus = FocusArea::Main;
                                    }
                                    _ => {}
                                },
                                _ => {}
                            }
                            continue;
                        }

                        if state.view == View::ProviderEdit {
                            match key.code {
                                KeyCode::Esc => state.view = View::ProviderList,
                                KeyCode::Tab | KeyCode::Down => {
                                    if state.edit_field_index < 4 {
                                        state.edit_field_index += 1;
                                    } else {
                                        state.focus = FocusArea::BottomBar;
                                        state.selected_f_key_index = 0;
                                    }
                                }
                                KeyCode::BackTab | KeyCode::Up => {
                                    if state.edit_field_index > 0 {
                                        state.edit_field_index -= 1;
                                    }
                                }
                                KeyCode::Enter => {
                                    if state.edit_field_index == 3 {
                                        state.view = View::ModelSelect;
                                        state.model_search_query.clear();
                                        state.update_filtered_models();
                                    } else {
                                        if let Some(p) = &state.editing_provider {
                                            let _ =
                                                self.tx_cmd.send(TuiCommand::SaveProvider(p.clone()));
                                            state.view = View::ProviderList;
                                        }
                                    }
                                }
                                KeyCode::Char(c) => {
                                    if state.edit_field_index != 3 {
                                        if let Some(p) = &mut state.editing_provider {
                                            update_provider_field(p, state.edit_field_index, c, false);
                                        }
                                    }
                                }
                                KeyCode::Backspace => {
                                    if state.edit_field_index != 3 {
                                        if let Some(p) = &mut state.editing_provider {
                                            update_provider_field(p, state.edit_field_index, ' ', true);
                                        }
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if state.view == View::JobCreate {
                            match key.code {
                                KeyCode::Esc | KeyCode::F(10) => state.view = View::Dashboard,
                                KeyCode::Tab => {
                                    state.job_create_field_idx = (state.job_create_field_idx + 1) % 3;
                                }
                                KeyCode::BackTab => {
                                    if state.job_create_field_idx == 0 {
                                        state.job_create_field_idx = 2;
                                    } else {
                                        state.job_create_field_idx -= 1;
                                    }
                                }
                                KeyCode::Up => {
                                    if state.job_create_field_idx == 2 {
                                        let i = match state.job_create_template_state.selected() {
                                            Some(i) => {
                                                if i == 0 {
                                                    state.templates.len().saturating_sub(1)
                                                } else {
                                                    i - 1
                                                }
                                            }
                                            None => 0,
                                        };
                                        state.job_create_template_state.select(Some(i));
                                    } else if state.job_create_field_idx > 0 {
                                        state.job_create_field_idx -= 1;
                                    }
                                }
                                KeyCode::Down => {
                                    if state.job_create_field_idx == 2 {
                                        let i = match state.job_create_template_state.selected() {
                                            Some(i) => {
                                                if i >= state.templates.len().saturating_sub(1) {
                                                    0
                                                } else {
                                                    i + 1
                                                }
                                            }
                                            None => 0,
                                        };
                                        state.job_create_template_state.select(Some(i));
                                    } else {
                                        state.job_create_field_idx += 1;
                                    }
                                }
                                KeyCode::Char(c) => {
                                    if state.job_create_field_idx == 0 {
                                        state.job_create_name.push(c);
                                    } else if state.job_create_field_idx == 1 {
                                        state.job_create_domain.push(c);
                                    }
                                }
                                KeyCode::Backspace => {
                                    if state.job_create_field_idx == 0 {
                                        state.job_create_name.pop();
                                    } else if state.job_create_field_idx == 1 {
                                        state.job_create_domain.pop();
                                    }
                                }
                                KeyCode::F(2) => {
                                    if !state.job_create_name.is_empty() && !state.job_create_domain.is_empty() {
                                        // Handled by common handler
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if state.view == View::ProviderTypeSelect {
                            match key.code {
                                KeyCode::Esc | KeyCode::F(10) => state.view = View::ProviderList,
                                KeyCode::Up => {
                                    if state.provider_type_idx > 0 {
                                        state.provider_type_idx -= 1;
                                    } else {
                                        state.provider_type_idx = 2; // Wrap
                                    }
                                }
                                KeyCode::Down => {
                                    if state.provider_type_idx < 2 {
                                        state.provider_type_idx += 1;
                                    } else {
                                        state.provider_type_idx = 0; // Wrap
                                    }
                                }
                                KeyCode::Enter => {
                                    let id = format!(
                                        "provider-{}",
                                        std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap()
                                            .as_secs()
                                    );
                                    let new_provider = match state.provider_type_idx {
                                        0 => ProviderConfig::OpenRouter {
                                            id,
                                            name: Some("New OpenRouter".to_string()),
                                            enabled: true,
                                            base_url: "https://openrouter.ai/api/v1".to_string(),
                                            api_key: "".to_string(),
                                            model: "openai/gpt-3.5-turbo".to_string(),
                                        },
                                        1 => ProviderConfig::Ollama {
                                            id,
                                            name: Some("New Ollama".to_string()),
                                            enabled: true,
                                            base_url: "http://localhost:11434".to_string(),
                                            model: "llama3".to_string(),
                                        },
                                        _ => ProviderConfig::Script {
                                            id,
                                            name: Some("New Script".to_string()),
                                            enabled: true,
                                            command: "./script.sh".to_string(),
                                            args: vec![],
                                            timeout_ms: Some(5000),
                                        },
                                    };
                                    state.editing_provider = Some(new_provider);
                                    state.view = View::ProviderEdit;
                                    state.edit_field_index = 0;
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if state.view == View::ModelSelect {
                            match key.code {
                                KeyCode::Esc => {
                                    if state.editing_provider.is_some() {
                                        state.view = View::ProviderEdit;
                                    } else {
                                        state.view = View::Settings;
                                    }
                                }
                                KeyCode::Tab => state.focus = FocusArea::BottomBar,
                                KeyCode::Enter => {
                                    let model_to_use = if let Some(selected_idx) = state.model_list_state.selected() {
                                        state.filtered_models.get(selected_idx).cloned()
                                    } else if !state.model_search_query.is_empty() {
                                        Some(state.model_search_query.clone())
                                    } else {
                                        None
                                    };

                                    if let Some(model) = model_to_use {
                                        if let Some(p) = &mut state.editing_provider {
                                            match p {
                                                ProviderConfig::OpenRouter { model: m, .. } => {
                                                    *m = model
                                                }
                                                ProviderConfig::Ollama { model: m, .. } => {
                                                    *m = model
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    if state.editing_provider.is_some() {
                                        state.view = View::ProviderEdit;
                                    } else {
                                        state.view = View::Settings;
                                    }
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

                        if state.view == View::ProviderList {
                            match key.code {
                                KeyCode::Esc => state.view = View::Dashboard,
                                KeyCode::Tab => state.focus = FocusArea::BottomBar,
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
                                            match p {
                                                ProviderConfig::OpenRouter { enabled, .. } => {
                                                    *enabled = !*enabled
                                                }
                                                ProviderConfig::Ollama { enabled, .. } => {
                                                    *enabled = !*enabled
                                                }
                                                ProviderConfig::Script { enabled, .. } => {
                                                    *enabled = !*enabled
                                                }
                                            }
                                            let _ = self
                                                .tx_cmd
                                                .send(TuiCommand::SaveProvider(p.clone()));
                                        }
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        match key.code {
                            KeyCode::Tab => state.focus = FocusArea::BottomBar,
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
                                    if let Some(job) = state.jobs.iter().find(|j| &j.id == job_id) {
                                        match job.status {
                                            destilation_core::domain::JobStatus::Running => {
                                                let _ = self
                                                    .tx_cmd
                                                    .send(TuiCommand::PauseJob(job_id.clone()));
                                            }
                                            destilation_core::domain::JobStatus::Paused => {
                                                let _ = self
                                                    .tx_cmd
                                                    .send(TuiCommand::ResumeJob(job_id.clone()));
                                            }
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
                    }
                    _ => {}
                }
            }
        }
    }
}

fn ui(f: &mut Frame, snap: &MetricsSnapshot, state: &mut TuiState) {
    let size = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(Color::Blue)),
        size,
    );

    let main_constraints = if matches!(state.view, View::Dashboard | View::JobDetail) {
        vec![
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ]
    } else {
        vec![
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ]
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(main_constraints)
        .split(f.area());

    let title_text = if state.is_done {
        "Destilation Agentic Pipeline [DONE]"
    } else {
        "Destilation Agentic Pipeline"
    };
    let title_style = if state.is_done {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    };

    let title = Paragraph::new(title_text)
        .style(title_style)
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(title, chunks[0]);

    let (content_chunk, footer_chunk) = if matches!(state.view, View::Dashboard | View::JobDetail) {
        let ratio = if snap.tasks_enqueued > 0 {
            (snap.tasks_persisted as f64 / snap.tasks_enqueued as f64).min(1.0)
        } else {
            0.0
        };
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .title("Total Progress")
                    .borders(Borders::ALL),
            )
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(ratio);
        f.render_widget(gauge, chunks[1]);
        (chunks[2], chunks[3])
    } else {
        (chunks[1], chunks[2])
    };

    match state.view {
        View::Dashboard => render_dashboard(f, content_chunk, snap, state),
        View::JobDetail => render_job_detail(f, content_chunk, state),
        View::Settings => render_settings(f, content_chunk),
        View::ModelSelect => render_model_select(f, content_chunk, state),
        View::ProviderList => render_provider_list(f, content_chunk, state),
        View::ProviderEdit => render_provider_edit(f, content_chunk, state),
        View::JobCreate => render_job_create(f, content_chunk, state),
        View::ProviderTypeSelect => render_provider_type_select(f, content_chunk, state),
    }

    let f_keys = get_f_keys(state.view);

    let visible_f_keys: Vec<(usize, &str, &str)> = f_keys
        .iter()
        .enumerate()
        .filter(|(_, (_, label))| !label.is_empty())
        .map(|(i, (key, label))| (i, *key, *label))
        .collect();

    let footer_constraints =
        vec![Constraint::Ratio(1, visible_f_keys.len() as u32); visible_f_keys.len()];
    let footer_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(footer_constraints)
        .split(footer_chunk);

    for (chunk_idx, (original_idx, key, label)) in visible_f_keys.iter().enumerate() {
        if chunk_idx >= footer_chunks.len() {
            break;
        }

        let is_selected =
            state.focus == FocusArea::BottomBar && state.selected_f_key_index == *original_idx;
        let style = if is_selected {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().bg(Color::Blue).fg(Color::White)
        };

        let btn = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", key),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(*label),
        ]))
        .style(style)
        .alignment(ratatui::layout::Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(if is_selected {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
        );

        f.render_widget(btn, footer_chunks[chunk_idx]);
    }

    if state.show_help {
        use ratatui::widgets::Clear;
        let block = Block::default()
            .title("Help")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::DarkGray));
        let area = centered_rect(60, 50, f.area());
        f.render_widget(Clear, area);
        f.render_widget(block, area);

        let help_text = vec![
            Line::from(Span::styled(
                "Navigation:",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Tab / Shift+Tab: Switch focus between Main View and Bottom Bar"),
            Line::from("  Arrows: Navigate lists and F-Key bar"),
            Line::from("  Enter: Select item or Trigger F-Key"),
            Line::from(""),
            Line::from(Span::styled(
                "Global Shortcuts:",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  F1: Toggle Help"),
            Line::from("  F10: Quit"),
            Line::from("  F11: Settings"),
            Line::from("  F12: Manage Providers"),
            Line::from(""),
            Line::from(Span::styled(
                "Provider Edit:",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Tab: Cycle fields"),
            Line::from("  Enter: Edit field / Save"),
            Line::from("  Space: Toggle Checkbox"),
        ];

        let paragraph = Paragraph::new(help_text).block(
            Block::default()
                .borders(Borders::NONE)
                .padding(ratatui::widgets::Padding::new(2, 2, 1, 1)),
        );

        f.render_widget(paragraph, area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_provider_list(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let items: Vec<ListItem> = state
        .providers
        .iter()
        .map(|p| {
            let name = p.name().cloned().unwrap_or_else(|| match p {
                ProviderConfig::OpenRouter { model, .. } => model.clone(),
                ProviderConfig::Ollama { model, .. } => model.clone(),
                ProviderConfig::Script { command, .. } => command.clone(),
            });
            let id = p.id();
            let status = if p.is_enabled() { "[x]" } else { "[ ]" };
            ListItem::new(format!("{} {} ({})", status, name, id))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title("Providers").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut state.provider_list_state);
}

fn render_provider_edit(f: &mut Frame, area: Rect, state: &mut TuiState) {
    if let Some(provider) = &state.editing_provider {
        let (name, base_url, api_key, model, enabled) = match provider {
            ProviderConfig::OpenRouter {
                name,
                base_url,
                api_key,
                model,
                enabled,
                ..
            } => (
                name.clone().unwrap_or_default(),
                base_url.clone(),
                api_key.clone(),
                model.clone(),
                *enabled,
            ),
            ProviderConfig::Ollama {
                name,
                base_url,
                model,
                enabled,
                ..
            } => (
                name.clone().unwrap_or_default(),
                base_url.clone(),
                "N/A".to_string(),
                model.clone(),
                *enabled,
            ),
            ProviderConfig::Script {
                name,
                command,
                enabled,
                ..
            } => (
                name.clone().unwrap_or_default(),
                command.clone(),
                "N/A".to_string(),
                "N/A".to_string(),
                *enabled,
            ),
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(area);

        let fields = vec![
            ("Name", name),
            ("Base URL / Command", base_url),
            ("API Key", api_key),
            ("Model", model),
            ("Enabled", enabled.to_string()),
        ];

        for (i, (label, value)) in fields.iter().enumerate() {
            if i >= chunks.len() {
                break;
            }
            let style = if state.edit_field_index == i {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let display_value = if i == 3 && value != "N/A" {
                if value.is_empty() {
                    "Press <Enter> to Select Model".to_string()
                } else {
                    format!("{} (Press <Enter> to Change)", value)
                }
            } else {
                format!("{}", value)
            };

            let p = Paragraph::new(display_value).block(
                Block::default()
                    .title(*label)
                    .borders(Borders::ALL)
                    .border_style(style),
            );
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
            Span::styled(
                "[c] Clean Database",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" (Wipes all jobs and tasks)"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Provider Config: "),
            Span::styled(
                "[o] OpenRouter Model",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];
    let block =
        Paragraph::new(text).block(Block::default().title("Settings").borders(Borders::ALL));
    f.render_widget(block, area);
}

fn render_model_select(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(5)].as_ref())
        .split(area);

    let search_text = format!("Search: {}", state.model_search_query);
    let search_block = Paragraph::new(search_text)
        .style(Style::default().fg(Color::Yellow))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Type to Filter"),
        );
    f.render_widget(search_block, chunks[0]);

    let items: Vec<ListItem> = state
        .filtered_models
        .iter()
        .map(|m| ListItem::new(Line::from(vec![Span::raw(m)])))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("Available Models")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");

    f.render_stateful_widget(list, chunks[1], &mut state.model_list_state);
}

fn render_dashboard(f: &mut Frame, area: Rect, snap: &MetricsSnapshot, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(area);

    let active_providers = state.providers.iter().filter(|p| p.is_enabled()).count();
    let total_providers = state.providers.len();

    let stats_text = vec![
        Line::from(vec![
            Span::raw("Active Providers: "),
            Span::styled(
                format!("{} / {}", active_providers, total_providers),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
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
    let stats = Paragraph::new(stats_text).block(
        Block::default()
            .title("Global Metrics")
            .borders(Borders::ALL),
    );
    f.render_widget(stats, chunks[0]);

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
                Span::raw(format!(
                    " - {} (Template: {})",
                    job.config.name, job.config.template_id
                )),
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

    let header_text = vec![
        Line::from(vec![
            Span::raw("ID: "),
            Span::styled(&job.id, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![Span::raw("Name: "), Span::raw(&job.config.name)]),
        Line::from(vec![
            Span::raw("Status: "),
            Span::raw(format!("{:?}", job.status)),
        ]),
        Line::from(vec![
            Span::raw("Tasks: "),
            Span::raw(format!("{}", tasks.len())),
        ]),
    ];
    let header = Paragraph::new(header_text)
        .block(Block::default().title("Job Details").borders(Borders::ALL));
    f.render_widget(header, chunks[0]);

    let items: Vec<ListItem> = tasks
        .iter()
        .map(|task| {
            let state_style = match task.state {
                destilation_core::domain::TaskState::Persisted => Style::default().fg(Color::Green),
                destilation_core::domain::TaskState::Rejected => Style::default().fg(Color::Red),
                destilation_core::domain::TaskState::Generating => {
                    Style::default().fg(Color::Yellow)
                }
                destilation_core::domain::TaskState::Validating => {
                    Style::default().fg(Color::Magenta)
                }
                _ => Style::default(),
            };

            ListItem::new(Line::from(vec![
                Span::raw(format!("Task {} ", task.id)),
                Span::styled(format!("{:?}", task.state), state_style),
                Span::raw(format!(" (Att: {})", task.attempts)),
            ]))
        })
        .collect();

    let list = List::new(items).block(Block::default().title("Tasks").borders(Borders::ALL));
    f.render_widget(list, chunks[1]);
}

fn update_provider_field(config: &mut ProviderConfig, index: usize, c: char, is_backspace: bool) {
    match config {
        ProviderConfig::OpenRouter {
            name,
            base_url,
            api_key,
            model,
            enabled,
            ..
        } => match index {
            0 => modify_string_opt(name, c, is_backspace),
            1 => modify_string(base_url, c, is_backspace),
            2 => modify_string(api_key, c, is_backspace),
            3 => modify_string(model, c, is_backspace),
            4 => {
                if !is_backspace && c == ' ' {
                    *enabled = !*enabled;
                }
            }
            _ => {}
        },
        ProviderConfig::Ollama {
            name,
            base_url,
            model,
            enabled,
            ..
        } => match index {
            0 => modify_string_opt(name, c, is_backspace),
            1 => modify_string(base_url, c, is_backspace),
            2 => {}
            3 => modify_string(model, c, is_backspace),
            4 => {
                if !is_backspace && c == ' ' {
                    *enabled = !*enabled;
                }
            }
            _ => {}
        },
        ProviderConfig::Script {
            name,
            command,
            enabled,
            ..
        } => match index {
            0 => modify_string_opt(name, c, is_backspace),
            1 => modify_string(command, c, is_backspace),
            4 => {
                if !is_backspace && c == ' ' {
                    *enabled = !*enabled;
                }
            }
            _ => {}
        },
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

fn render_job_create(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3), // Name
                Constraint::Length(3), // Domain/Subject
                Constraint::Min(5),    // Template List
            ]
            .as_ref(),
        )
        .split(area);

    // Job Name Input
    let name_style = if state.job_create_field_idx == 0 {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let name_input = Paragraph::new(state.job_create_name.as_str()).block(
        Block::default()
            .title("Job Name")
            .borders(Borders::ALL)
            .border_style(name_style),
    );
    f.render_widget(name_input, chunks[0]);

    // Domain/Subject Input
    let domain_style = if state.job_create_field_idx == 1 {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let domain_input = Paragraph::new(state.job_create_domain.as_str()).block(
        Block::default()
            .title("Subject / Domain (e.g., 'Python Coding', 'Creative Writing')")
            .borders(Borders::ALL)
            .border_style(domain_style),
    );
    f.render_widget(domain_input, chunks[1]);

    // Template Selection
    let template_style = if state.job_create_field_idx == 2 {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let items: Vec<ListItem> = state
        .templates
        .iter()
        .map(|t| ListItem::new(format!("{} - {}", t.id, t.name)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("Select Template")
                .borders(Borders::ALL)
                .border_style(template_style),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, chunks[2], &mut state.job_create_template_state);
}

fn render_provider_type_select(f: &mut Frame, area: Rect, state: &TuiState) {
    let area = centered_rect(60, 40, area);
    let items = vec![
        ListItem::new("OpenRouter"),
        ListItem::new("Ollama"),
        ListItem::new("Script"),
    ];
    let list = List::new(items)
        .block(Block::default().title("Select Provider Type").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut list_state = ListState::default();
    list_state.select(Some(state.provider_type_idx));

    f.render_widget(ratatui::widgets::Clear, area);
    f.render_stateful_widget(list, area, &mut list_state);
}

fn get_f_keys(view: View) -> Vec<(&'static str, &'static str)> {
    match view {
        View::Dashboard => vec![
            ("F1", "Help"),
            ("F2", "Run/Ps"),
            ("F3", "View"),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", "New"),
            ("F8", "Del"),
            ("F9", ""),
            ("F10", "Quit"),
            ("F11", "Set"),
            ("F12", "Prov"),
        ],
        View::JobCreate => vec![
            ("F1", "Help"),
            ("F2", "Start"),
            ("F3", ""),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", ""),
            ("F10", "Cancel"),
            ("F11", ""),
            ("F12", ""),
        ],
        View::ProviderTypeSelect => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", ""),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", "Back"),
            ("F10", "Cancel"),
            ("F11", ""),
            ("F12", ""),
        ],
        View::ProviderList => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", ""),
            ("F4", "Edit"),
            ("F5", ""),
            ("F6", ""),
            ("F7", "New"),
            ("F8", "Del"),
            ("F9", ""),
            ("F10", "Quit"),
            ("F11", "Set"),
            ("F12", ""),
        ],
        View::ProviderEdit => vec![
            ("F1", "Help"),
            ("F2", "Save"),
            ("F3", ""),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", "Back"),
            ("F10", "Quit"),
            ("F11", "Set"),
            ("F12", ""),
        ],
        View::ModelSelect => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", ""),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", "Back"),
            ("F10", "Quit"),
            ("F11", ""),
            ("F12", ""),
        ],
        _ => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", "Dash"),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", ""),
            ("F10", "Quit"),
            ("F11", "Set"),
            ("F12", "Prov"),
        ],
    }
}

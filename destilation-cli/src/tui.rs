use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use destilation_core::domain::{Job, JobConfig, ProviderId, Task, TemplateConfig};
use destilation_core::logging::LogEvent;
use destilation_core::metrics::{Metrics, MetricsSnapshot};
use destilation_core::provider::ProviderConfig;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
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
    PauseJob(String),
    ResumeJob(String),
    DeleteJob(String),
    SaveProvider(ProviderConfig),
    TestProvider(ProviderConfig),
    DeleteProvider(ProviderId),
    SaveTemplate(TemplateConfig),
    DeleteTemplate(destilation_core::domain::TemplateId),
}

pub enum TuiUpdate {
    Jobs(Vec<Job>),
    Tasks(JobId, Vec<Task>),
    Providers(Vec<ProviderConfig>),
    Templates(Vec<TemplateConfig>),
    LogEvents(Vec<LogEvent>),
    ErrorPopup(String, String), // (job_id, error_message)
    ProviderTestResult(String),
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
    show_logs: bool,

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
    template_list_state: ListState,
    job_create_name: String,
    job_create_domain: String,
    job_create_samples: String,
    job_create_template_state: ListState,
    job_create_field_idx: usize,

    // Provider Creation State
    provider_type_idx: usize,

    // Global Navigation State
    focus: FocusArea,
    selected_f_key_index: usize,

    editing_template: Option<TemplateConfig>,
    template_edit_field_idx: usize,
    template_preview_output: Option<String>,

    dataset_root: String,
    job_logs: HashMap<JobId, Vec<LogEvent>>,
    global_logs: Vec<LogEvent>,
    log_list_state: ListState,

    // Session-based metrics for accurate throughput
    samples_at_start: u64,
    tasks_at_start: u64,
    session_started_at: Option<std::time::Instant>,

    // Status + activity indicator
    last_status_message: String,
    last_status_level: Option<destilation_core::logging::LogLevel>,
    last_activity_at: Option<std::time::Instant>,
    spinner_tick: usize,
    
    // Error popup
    error_popup: Option<(String, String)>, // (job_id, error_message)
    provider_test_popup: Option<String>,
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
    LogViewer,
    TemplateList,
    TemplateEdit,
}

#[derive(PartialEq, Clone, Copy)]
enum FocusArea {
    Main,
    BottomBar,
}

impl TuiState {
    fn new() -> Self {
        Self {
            jobs: Vec::new(),
            tasks: HashMap::new(),
            job_list_state: ListState::default(),
            selected_job_id: None,
            view: View::Dashboard,
            show_help: false,
            is_done: false,
            show_logs: false,

            providers: Vec::new(),
            provider_list_state: ListState::default(),
            editing_provider: None,
            edit_field_index: 0,

            model_search_query: String::new(),
            filtered_models: Vec::new(),
            available_models: Self::default_available_models(),
            model_list_state: ListState::default(),

            templates: Vec::new(),
            template_list_state: ListState::default(),
            job_create_name: String::new(),
            job_create_domain: String::new(),
            job_create_samples: "100".to_string(),
            job_create_template_state: ListState::default(),
            job_create_field_idx: 0,

            provider_type_idx: 0,

            editing_template: None,
            template_edit_field_idx: 0,
            template_preview_output: None,

            focus: FocusArea::Main,
            selected_f_key_index: 0,

            dataset_root: "datasets/default".to_string(),
            job_logs: HashMap::new(),
            global_logs: Vec::new(),
            log_list_state: ListState::default(),

            samples_at_start: 0,
            tasks_at_start: 0,
            session_started_at: None,
            last_status_message: "Idle".to_string(),
            last_status_level: None,
            last_activity_at: None,
            spinner_tick: 0,
            error_popup: None,
            provider_test_popup: None,
        }
    }

    fn record_activity(&mut self) {
        self.last_activity_at = Some(std::time::Instant::now());
    }

    fn push_log_events(&mut self, events: Vec<LogEvent>) {
        if let Some(last) = events.last() {
            self.last_status_message = format_status_message(last);
            self.last_status_level = Some(last.level);
            self.record_activity();
        }
        for ev in events {
            if let Some(job_id) = ev.job_id.clone() {
                let buf = self.job_logs.entry(job_id).or_default();
                buf.push(ev);
                if buf.len() > 500 {
                    let drop = buf.len() - 500;
                    buf.drain(0..drop);
                }
            } else {
                self.global_logs.push(ev);
                if self.global_logs.len() > 500 {
                    let drop = self.global_logs.len() - 500;
                    self.global_logs.drain(0..drop);
                }
            }
        }
    }

    fn default_available_models() -> Vec<String> {
        vec![
            // Free models (verified from OpenRouter API)
            "deepseek/deepseek-r1-0528:free".to_string(),
            "qwen/qwen3-next-80b-a3b-instruct:free".to_string(),
            "nvidia/nemotron-nano-12b-v2-vl:free".to_string(),
            "nvidia/nemotron-nano-9b-v2:free".to_string(),
            "qwen/qwen3-coder:free".to_string(),
            "google/gemma-3n-e4b-it:free".to_string(),
            "google/gemma-3n-e2b-it:free".to_string(),
            "mistralai/devstral-2512:free".to_string(),
            // Paid models
            "openai/gpt-4o".to_string(),
            "openai/gpt-4o-mini".to_string(),
            "anthropic/claude-3.5-sonnet".to_string(),
            "google/gemini-flash-1.5".to_string(),
            "meta-llama/llama-3-70b-instruct".to_string(),
        ]
    }

    /// Fetch available models from Ollama server
    async fn fetch_ollama_models(base_url: &str) -> Vec<String> {
        let client = reqwest::ClientBuilder::new()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
        
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        if let Some(models) = body.get("models").and_then(|m| m.as_array()) {
                            models
                                .iter()
                                .filter_map(|model| model.get("name").and_then(|n| n.as_str()))
                                .map(|name| name.to_string())
                                .collect()
                        } else {
                            vec!["llama3".to_string()] // Fallback if response structure is unexpected
                        }
                    }
                    Err(e) => {
                        println!("Error parsing Ollama models response: {}", e);
                        vec!["llama3".to_string()]
                    }
                }
            }
            Ok(resp) => {
                println!("Ollama API returned status: {}", resp.status());
                vec!["llama3".to_string()]
            }
            Err(e) => {
                println!("Error fetching Ollama models: {}", e);
                vec!["llama3".to_string()]
            }
        }
    }

    async fn set_available_models_for_provider(&mut self) {
        self.available_models = if let Some(p) = &self.editing_provider {
            match p {
                ProviderConfig::Ollama { base_url, .. } => {
                    println!("Fetching Ollama models from: {}", base_url);
                    let models = Self::fetch_ollama_models(base_url).await;
                    println!("Found {} Ollama models: {:?}", models.len(), models);
                    models
                }
                ProviderConfig::OpenRouter { .. } => Self::default_available_models(),
                _ => Vec::new(),
            }
        } else {
            Self::default_available_models()
        };
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
        self.sync_selected_job_id();
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
        self.sync_selected_job_id();
    }

    fn sync_selected_job_id(&mut self) {
        let selected = self
            .job_list_state
            .selected()
            .and_then(|i| self.jobs.get(i))
            .map(|j| j.id.clone());
        self.selected_job_id = selected;
    }

    fn selected_job_id_or_highlighted(&self) -> Option<JobId> {
        if let Some(id) = &self.selected_job_id {
            return Some(id.clone());
        }
        self.job_list_state
            .selected()
            .and_then(|i| self.jobs.get(i))
            .map(|j| j.id.clone())
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
        if let Some(id) = &self.selected_job_id {
            if let Some(pos) = self.jobs.iter().position(|j| &j.id == id) {
                self.job_list_state.select(Some(pos));
            }
        } else if !self.jobs.is_empty() {
            self.job_list_state.select(Some(0));
            self.sync_selected_job_id();
        }
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
        dataset_root: String,
    ) -> Self {
        let mut state = TuiState::new();
        state.dataset_root = dataset_root;
        Self {
            metrics,
            rx,
            tx_cmd,
            state,
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
                        if self.state.selected_job_id.is_none() {
                            self.state.sync_selected_job_id();
                        } else if let Some(id) = &self.state.selected_job_id {
                            if let Some(pos) = self.state.jobs.iter().position(|j| &j.id == id) {
                                self.state.job_list_state.select(Some(pos));
                            } else if !self.state.jobs.is_empty() {
                                self.state.job_list_state.select(Some(0));
                                self.state.sync_selected_job_id();
                            } else {
                                self.state.selected_job_id = None;
                                self.state.job_list_state.select(None);
                            }
                        }
                    }
                    TuiUpdate::Tasks(job_id, tasks) => {
                        self.state.tasks.insert(job_id, tasks);
                        self.state.last_status_message = "Tasks updated".to_string();
                        self.state.last_status_level = Some(destilation_core::logging::LogLevel::Info);
                        self.state.record_activity();
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
                    TuiUpdate::LogEvents(events) => {
                        self.state.push_log_events(events);
                    }
                    TuiUpdate::ErrorPopup(job_id, error_message) => {
                        self.state.error_popup = Some((job_id, error_message));
                    }
                    TuiUpdate::ProviderTestResult(message) => {
                        self.state.provider_test_popup = Some(message);
                    }
                }
            }

            let snap = self.metrics.snapshot();
            self.state.spinner_tick = self.state.spinner_tick.wrapping_add(1);
            
            // Session logic: if something is running and we don't have a session, start one.
            // If nothing is running, clear the session tracker.
            let any_running = self.state.jobs.iter().any(|j| j.status == destilation_core::domain::JobStatus::Running);
            if any_running {
                if self.state.session_started_at.is_none() {
                    self.state.session_started_at = Some(std::time::Instant::now());
                    self.state.samples_at_start = snap.samples_persisted;
                    self.state.tasks_at_start = snap.tasks_persisted;
                }
            } else {
                self.state.session_started_at = None;
            }

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
                                                    if !state.job_create_name.is_empty()
                                                        && !state.job_create_domain.is_empty()
                                                    {
                                                        if let Some(idx) = state
                                                            .job_create_template_state
                                                            .selected()
                                                        {
                                                            if let Some(template) =
                                                                state.templates.get(idx)
                                                            {
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
                                                                    target_samples: state.job_create_samples.parse().unwrap_or(10),
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
                                                                let _ = self
                                                                    .tx_cmd
                                                                    .send(TuiCommand::StartJob(config));
                                                                state.view = View::Dashboard;
                                                            }
                                                        }
                                                    }
                                                } else if state.view == View::Dashboard {
                                                    if let Some(job_id) =
                                                        state.selected_job_id_or_highlighted()
                                                    {
                                                        state.selected_job_id =
                                                            Some(job_id.clone());
                                                        if let Some(job) = state
                                                            .jobs
                                                            .iter()
                                                            .find(|j| j.id == job_id)
                                                        {
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
                                                    if let Some(job_id) =
                                                        state.selected_job_id_or_highlighted()
                                                    {
                                                        let _ = self.tx_cmd.send(
                                                            TuiCommand::DeleteJob(job_id.clone()),
                                                        );
                                                        state.selected_job_id = None;
                                                    }
                                                }
                                            }
                                            9 => return Ok(()),
                                            10 => {
                                                state.view = View::Settings;
                                                state.focus = FocusArea::Main;
                                            }
                                            11 => {
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
                                                    // Fetch Ollama models
                                                    let rt = tokio::runtime::Runtime::new().unwrap();
                                                    rt.block_on(state.set_available_models_for_provider());

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
                        // Error popup takes priority
                        if state.error_popup.is_some() {
                            if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                                state.error_popup = None;
                            }
                            continue;
                        }
                        if state.provider_test_popup.is_some() {
                            if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                                state.provider_test_popup = None;
                            }
                            continue;
                        }
                        
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
                                    if !state.job_create_name.is_empty()
                                        && !state.job_create_domain.is_empty()
                                    {
                                        if let Some(idx) =
                                            state.job_create_template_state.selected()
                                        {
                                            if let Some(template) = state.templates.get(idx) {
                                                let providers: Vec<
                                                    destilation_core::domain::JobProviderSpec,
                                                > = state
                                                    .providers
                                                    .iter()
                                                    .filter(|p| p.is_enabled())
                                                    .map(|p| {
                                                        destilation_core::domain::JobProviderSpec {
                                                            provider_id: p.id().clone(),
                                                            weight: 1.0,
                                                            capabilities_required: vec![],
                                                        }
                                                    })
                                                    .collect();

                                                let domains =
                                                    vec![destilation_core::domain::DomainSpec {
                                                        id: format!(
                                                            "domain-{}",
                                                            std::time::SystemTime::now()
                                                                .duration_since(
                                                                    std::time::UNIX_EPOCH
                                                                )
                                                                .unwrap()
                                                                .as_secs()
                                                        ),
                                                        name: state.job_create_domain.clone(),
                                                        weight: 1.0,
                                                        tags: vec![],
                                                    }];

                                                let job_id = format!(
                                                    "job-{}",
                                                    std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap()
                                                        .as_secs()
                                                );

                                                let config = JobConfig {
                                                    id: job_id.clone(),
                                                    name: state.job_create_name.clone(),
                                                    description: None,
                                                    target_samples: state.job_create_samples.parse().unwrap_or(10),
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
                                                        dataset_dir: format!(
                                                            "{}/{}",
                                                            state.dataset_root, job_id
                                                        ),
                                                        shard_size: 1000,
                                                        compress: false,
                                                        metadata: HashMap::new(),
                                                    },
                                                };
                                                let _ =
                                                    self.tx_cmd.send(TuiCommand::StartJob(config));
                                                state.view = View::Dashboard;
                                            }
                                        }
                                    }
                                } else if state.view == View::Dashboard {
                                    if let Some(job_id) = state.selected_job_id_or_highlighted() {
                                        state.selected_job_id = Some(job_id.clone());
                                        if let Some(job) =
                                            state.jobs.iter().find(|j| j.id == job_id)
                                        {
                                            match job.status {
                                                destilation_core::domain::JobStatus::Running => {
                                                    let _ = self
                                                        .tx_cmd
                                                        .send(TuiCommand::PauseJob(job_id.clone()));
                                                }
                                                destilation_core::domain::JobStatus::Paused
                                                | destilation_core::domain::JobStatus::Pending => {
                                                    let _ = self.tx_cmd.send(
                                                        TuiCommand::ResumeJob(job_id.clone()),
                                                    );
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
                            KeyCode::F(5) => {
                                if state.view == View::ProviderEdit {
                                    if let Some(p) = &state.editing_provider {
                                        let _ =
                                            self.tx_cmd.send(TuiCommand::TestProvider(p.clone()));
                                    }
                                } else if state.view == View::Dashboard || state.view == View::JobDetail {
                                    state.view = View::LogViewer;
                                    state.focus = FocusArea::Main;
                                }
                            }
                            KeyCode::F(6) => {
                                if state.view == View::Dashboard || state.view == View::JobDetail {
                                    state.view = View::TemplateList;
                                    state.focus = FocusArea::Main;
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
                                        !state
                                            .providers
                                            .iter()
                                            .any(|existing| existing.id() == p.id())
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
                                } else if state.view == View::LogViewer {
                                    state.view = View::Dashboard;
                                } else if state.view == View::TemplateList {
                                    state.view = View::Dashboard;
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
                                View::Dashboard => vec![0, 1, 2, 5, 6, 7, 9, 10, 11],
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
                                KeyCode::Enter => {
                                    match state.selected_f_key_index {
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
                                                if !state.job_create_name.is_empty()
                                                    && !state.job_create_domain.is_empty()
                                                {
                                                    if let Some(idx) =
                                                        state.job_create_template_state.selected()
                                                    {
                                                        if let Some(template) =
                                                            state.templates.get(idx)
                                                        {
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
                                                            target_samples: state.job_create_samples.parse().unwrap_or(10),
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
                                                            let _ = self
                                                                .tx_cmd
                                                                .send(TuiCommand::StartJob(config));
                                                            state.view = View::Dashboard;
                                                        }
                                                    }
                                                }
                                            } else if state.view == View::Dashboard {
                                                if let Some(job_id) = &state.selected_job_id {
                                                    if let Some(job) =
                                                        state.jobs.iter().find(|j| &j.id == job_id)
                                                    {
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
                                        8 => {
                                            // F9 Back
                                            if state.view == View::ProviderEdit {
                                                let is_new =
                                                    if let Some(p) = &state.editing_provider {
                                                        !state
                                                            .providers
                                                            .iter()
                                                            .any(|existing| existing.id() == p.id())
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
                                    }
                                }
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
                                        // Fetch Ollama models
                                        if state.editing_provider.is_some() {
                                            // We need to run async code from sync context
                                            let rt = tokio::runtime::Runtime::new().unwrap();
                                            rt.block_on(state.set_available_models_for_provider());
                                            
                                            state.view = View::ModelSelect;
                                            state.model_search_query.clear();
                                            state.update_filtered_models();
                                        }
                                    } else if let Some(p) = &state.editing_provider {
                                        let _ =
                                            self.tx_cmd.send(TuiCommand::SaveProvider(p.clone()));
                                        state.view = View::ProviderList;
                                    }
                                }
                                KeyCode::Char(c) => {
                                    if state.edit_field_index != 3 {
                                        if let Some(p) = &mut state.editing_provider {
                                            update_provider_field(
                                                p,
                                                state.edit_field_index,
                                                c,
                                                false,
                                            );
                                        }
                                    }
                                }
                                KeyCode::Backspace => {
                                    if state.edit_field_index != 3 {
                                        if let Some(p) = &mut state.editing_provider {
                                            update_provider_field(
                                                p,
                                                state.edit_field_index,
                                                ' ',
                                                true,
                                            );
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
                                    state.job_create_field_idx =
                                        (state.job_create_field_idx + 1) % 4;
                                }
                                KeyCode::BackTab => {
                                    if state.job_create_field_idx == 0 {
                                        state.job_create_field_idx = 3;
                                    } else {
                                        state.job_create_field_idx -= 1;
                                    }
                                }
                                KeyCode::Up => {
                                    if state.job_create_field_idx == 3 {
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
                                    if state.job_create_field_idx == 3 {
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
                                    } else if state.job_create_field_idx == 2 && c.is_ascii_digit()
                                    {
                                        state.job_create_samples.push(c);
                                    }
                                }
                                KeyCode::Backspace => {
                                    if state.job_create_field_idx == 0 {
                                        state.job_create_name.pop();
                                    } else if state.job_create_field_idx == 1 {
                                        state.job_create_domain.pop();
                                    } else if state.job_create_field_idx == 2 {
                                        state.job_create_samples.pop();
                                    }
                                }
                                KeyCode::F(2) => {
                                    // Handled by common handler, but we need to ensure validation passes
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
                                    let model_to_use = if let Some(selected_idx) =
                                        state.model_list_state.selected()
                                    {
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
                        if state.view == View::TemplateList {
                            match key.code {
                                KeyCode::Esc | KeyCode::F(9) => state.view = View::Dashboard,
                                KeyCode::Tab => state.focus = FocusArea::BottomBar,
                                KeyCode::Enter => {
                                    if let Some(i) = state.template_list_state.selected() {
                                        if let Some(t) = state.templates.get(i) {
                                            state.editing_template = Some(t.clone());
                                            state.view = View::TemplateEdit;
                                            state.template_edit_field_idx = 0;
                                        }
                                    }
                                }
                                KeyCode::F(7) => {
                                    state.editing_template = Some(destilation_core::domain::TemplateConfig {
                                        id: format!("template-{}", rand::random::<u32>()),
                                        name: "New Template".to_string(),
                                        description: String::new(),
                                        mode: destilation_core::domain::TemplateMode::Simple,
                                        schema: destilation_core::domain::TemplateSchema {
                                            version: "1.0.0".to_string(),
                                            fields: vec![],
                                            json_schema: None,
                                        },
                                        system_prompt: String::new(),
                                        user_prompt_pattern: "{{prompt}}".to_string(),
                                        examples: vec![],
                                        validators: vec![],
                                    });
                                    state.view = View::TemplateEdit;
                                    state.template_edit_field_idx = 0;
                                }
                                KeyCode::F(8) => {
                                    if let Some(i) = state.template_list_state.selected() {
                                        if let Some(t) = state.templates.get(i) {
                                            let _ = self.tx_cmd.send(TuiCommand::DeleteTemplate(t.id.clone()));
                                        }
                                    }
                                }
                                _ => {}
                            }
                            continue;
                        }

                        if state.view == View::TemplateEdit {
                            match key.code {
                                KeyCode::Esc | KeyCode::F(9) => {
                                    if state.template_preview_output.is_some() {
                                        state.template_preview_output = None;
                                    } else {
                                        state.view = View::TemplateList;
                                    }
                                }
                                KeyCode::Tab => {
                                    state.template_edit_field_idx = (state.template_edit_field_idx + 1) % 6;
                                }
                                KeyCode::BackTab => {
                                    state.template_edit_field_idx = (state.template_edit_field_idx + 5) % 6;
                                }
                                KeyCode::F(5) => {
                                    if let Some(t) = &state.editing_template {
                                        let dummy_user_prompt = t.user_prompt_pattern.replace("{{prompt}}", "Explain quantum entanglement in simple terms.");
                                        let dummy_response = r#"{
  "explanation": "Quantum entanglement is a phenomenon where two particles become linked...",
  "status": "success",
  "metadata": { "model": "gpt-4o", "confidence": 0.98 }
}"#;
                                        state.template_preview_output = Some(format!(
                                            "SYSTEM: {}\n\nUSER: {}\n\nRESPONSE:\n{}",
                                            t.system_prompt, dummy_user_prompt, dummy_response
                                        ));
                                    }
                                }
                                KeyCode::Enter | KeyCode::F(10) => {
                                    if let Some(template) = &state.editing_template {
                                        let _ = self.tx_cmd.send(TuiCommand::SaveTemplate(template.clone()));
                                        state.view = View::TemplateList;
                                    }
                                }
                                KeyCode::Char(c) => {
                                    if let Some(t) = &mut state.editing_template {
                                        match state.template_edit_field_idx {
                                            0 => t.id.push(c),
                                            1 => t.name.push(c),
                                            2 => t.description.push(c),
                                            3 => {
                                                if c == ' ' {
                                                    t.mode = match t.mode {
                                                        destilation_core::domain::TemplateMode::Simple => destilation_core::domain::TemplateMode::Reasoning,
                                                        destilation_core::domain::TemplateMode::Reasoning => destilation_core::domain::TemplateMode::Moe,
                                                        destilation_core::domain::TemplateMode::Moe => destilation_core::domain::TemplateMode::Tools,
                                                        destilation_core::domain::TemplateMode::Tools => destilation_core::domain::TemplateMode::Quad,
                                                        destilation_core::domain::TemplateMode::Quad => destilation_core::domain::TemplateMode::Preference,
                                                        destilation_core::domain::TemplateMode::Preference => destilation_core::domain::TemplateMode::ToolTrace,
                                                        destilation_core::domain::TemplateMode::ToolTrace => destilation_core::domain::TemplateMode::Custom,
                                                        destilation_core::domain::TemplateMode::Custom => destilation_core::domain::TemplateMode::Simple,
                                                    };
                                                }
                                            }
                                            4 => t.system_prompt.push(c),
                                            5 => t.user_prompt_pattern.push(c),
                                            _ => {}
                                        }
                                    }
                                }
                                KeyCode::Backspace => {
                                    if let Some(t) = &mut state.editing_template {
                                        match state.template_edit_field_idx {
                                            0 => { t.id.pop(); }
                                            1 => { t.name.pop(); }
                                            2 => { t.description.pop(); }
                                            4 => { t.system_prompt.pop(); }
                                            5 => { t.user_prompt_pattern.pop(); }
                                            _ => {}
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
                                if let Some(job_id) = state.selected_job_id_or_highlighted() {
                                    let _ = self.tx_cmd.send(TuiCommand::DeleteJob(job_id.clone()));
                                    state.view = View::Dashboard;
                                    state.selected_job_id = None;
                                }
                            }
                            KeyCode::Char('p') => {
                                if let Some(job_id) = state.selected_job_id_or_highlighted() {
                                    state.selected_job_id = Some(job_id.clone());
                                    if let Some(job) = state.jobs.iter().find(|j| j.id == job_id) {
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
                            }
                            KeyCode::Char('o') if state.view == View::Settings => {
                                state.view = View::ModelSelect;
                                state.model_search_query.clear();
                                state.update_filtered_models();
                            }
                            KeyCode::Down => {
                                if state.view == View::LogViewer {
                                    let i = match state.log_list_state.selected() {
                                        Some(i) => i + 1,
                                        None => 0,
                                    };
                                    state.log_list_state.select(Some(i));
                                } else if state.view == View::TemplateList {
                                    let i = match state.template_list_state.selected() {
                                        Some(i) => {
                                            if i >= state.templates.len().saturating_sub(1) {
                                                0
                                            } else {
                                                i + 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.template_list_state.select(Some(i));
                                } else {
                                    state.next_job()
                                }
                            }
                            KeyCode::Up => {
                                if state.view == View::LogViewer {
                                    let i = match state.log_list_state.selected() {
                                        Some(i) => i.saturating_sub(1),
                                        None => 0,
                                    };
                                    state.log_list_state.select(Some(i));
                                } else if state.view == View::TemplateList {
                                    let i = match state.template_list_state.selected() {
                                        Some(i) => {
                                            if i == 0 {
                                                state.templates.len().saturating_sub(1)
                                            } else {
                                                i - 1
                                            }
                                        }
                                        None => 0,
                                    };
                                    state.template_list_state.select(Some(i));
                                } else {
                                    state.previous_job()
                                }
                            }
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

    let (generating, validating, queued) = in_flight_counts(state);
    let show_spinner = state
        .last_activity_at
        .map(|t| t.elapsed().as_secs_f32() <= 3.0)
        .unwrap_or(false)
        || generating > 0
        || validating > 0;
    let spinner = if show_spinner {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        frames[state.spinner_tick % frames.len()]
    } else {
        " "
    };
    let status_color = match state.last_status_level {
        Some(destilation_core::logging::LogLevel::Error) => Color::Red,
        Some(destilation_core::logging::LogLevel::Warn) => Color::Yellow,
        Some(destilation_core::logging::LogLevel::Info) => Color::Green,
        Some(destilation_core::logging::LogLevel::Debug) => Color::Cyan,
        Some(destilation_core::logging::LogLevel::Trace) => Color::Gray,
        None => Color::White,
    };
    let mut total_received = 0u64;
    let mut total_saved = 0u64;
    let mut total_rejected = 0u64;
    for tasks in state.tasks.values() {
        for task in tasks {
            if task.raw_response.is_some() {
                total_received += 1;
            }
            match task.state {
                destilation_core::domain::TaskState::Persisted => total_saved += 1,
                destilation_core::domain::TaskState::Rejected => total_rejected += 1,
                _ => {}
            }
        }
    }

    let status_block = Block::default().borders(Borders::ALL).title("Status");
    f.render_widget(status_block.clone(), chunks[0]);
    let status_inner = status_block.inner(chunks[0]);
    let status_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)].as_ref())
        .split(status_inner);

    let title_line = Line::from(Span::styled(title_text, title_style));
    let title_widget = Paragraph::new(title_line).alignment(Alignment::Left);
    f.render_widget(title_widget, status_chunks[0]);

    let status_line = Line::from(vec![
        Span::styled(state.last_status_message.as_str(), Style::default().fg(status_color)),
        Span::raw(format!(
            " | gen={} val={} queued={} | recv={} saved={} rej={} | ",
            generating,
            validating,
            queued,
            total_received,
            total_saved,
            total_rejected
        )),
        Span::styled(spinner, Style::default().fg(status_color)),
    ]);
    let status_widget = Paragraph::new(status_line).alignment(Alignment::Right);
    f.render_widget(status_widget, status_chunks[1]);

    if let Some(message) = &state.provider_test_popup {
        use ratatui::widgets::Clear;
        let block = Block::default()
            .title("Provider Test")
            .borders(Borders::ALL)
            .style(Style::default().fg(Color::Cyan));
        let popup_area = centered_rect(70, 40, f.area());
        f.render_widget(Clear, popup_area);
        let p = Paragraph::new(message.as_str())
            .block(block)
            .wrap(Wrap { trim: true });
        f.render_widget(p, popup_area);
        return;
    }

    let (content_chunk, footer_chunk) = if matches!(state.view, View::Dashboard | View::JobDetail) {
        let active_job = state.jobs.iter().find(|j| j.status == destilation_core::domain::JobStatus::Running)
            .or_else(|| state.selected_job_id.as_ref().and_then(|id| state.jobs.iter().find(|j| &j.id == id)));
        
        let (ratio, label) = if let Some(job) = active_job {
            let tasks = state.tasks.get(&job.id).map(|v| v.as_slice()).unwrap_or(&[]);
            let saved = tasks.iter().filter(|t| t.state == destilation_core::domain::TaskState::Persisted).count() as u64;
            let target = job.config.target_samples.max(1);
            let pct = (saved as f64 / target as f64).min(1.0);
            (pct, format!("{}: {} / {} ({}%)", job.config.name, saved, target, (pct * 100.0) as u64))
        } else {
            (0.0, "No active or selected job".to_string())
        };

        let gauge = Gauge::default()
            .block(
                Block::default()
                    .title("Job Progress")
                    .borders(Borders::ALL),
            )
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(ratio)
            .label(label);
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
        View::LogViewer => render_log_viewer(f, content_chunk, state),
        View::TemplateList => render_template_list(f, content_chunk, state),
        View::TemplateEdit => render_template_edit(f, content_chunk, state),
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
    
    // Error popup (takes priority over help)
    if let Some((job_id, error_message)) = &state.error_popup {
        use ratatui::widgets::Clear;
        let block = Block::default()
            .title("⚠ API Error - Job Paused")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Red).fg(Color::White));
        let area = centered_rect(70, 40, f.area());
        f.render_widget(Clear, area);
        f.render_widget(block, area);

        let error_text = vec![
            Line::from(Span::styled(
                format!("Job: {}", job_id),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Error Message:",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(error_message.as_str()),
            Line::from(""),
            Line::from("The job has been automatically paused to prevent API bans."),
            Line::from("Please fix the issue before resuming."),
            Line::from(""),
            Line::from(Span::styled(
                "Press ESC or Enter to dismiss",
                Style::default().add_modifier(Modifier::ITALIC),
            )),
        ];

        let paragraph = Paragraph::new(error_text)
            .wrap(Wrap { trim: true })
            .block(
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

fn render_template_list(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let items: Vec<ListItem> = state
        .templates
        .iter()
        .map(|t| {
            let name = &t.name;
            let id = &t.id;
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<20}", name), Style::default().fg(Color::Cyan)),
                Span::raw(" "),
                Span::styled(format!("({})", id), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title("Templates (F7: New, F8: Delete, Enter: Edit)")
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut state.template_list_state);
}
fn render_template_edit(f: &mut Frame, area: Rect, state: &mut TuiState) {
    if let Some(template) = &state.editing_template {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(3), // ID
                    Constraint::Length(3), // Name
                    Constraint::Length(3), // Description
                    Constraint::Length(3), // Mode
                    Constraint::Length(10), // System Prompt
                    Constraint::Min(5),    // User Prompt Pattern
                ]
                .as_ref(),
            )
            .split(area);

        let fields = [
            ("ID", template.id.as_str()),
            ("Name", template.name.as_str()),
            ("Description", template.description.as_str()),
            (
                "Mode",
                match template.mode {
                    destilation_core::domain::TemplateMode::Simple => "Simple",
                    destilation_core::domain::TemplateMode::Reasoning => "Reasoning",
                    destilation_core::domain::TemplateMode::Moe => "MOE",
                    destilation_core::domain::TemplateMode::Tools => "Tools",
                    destilation_core::domain::TemplateMode::Quad => "Quad",
                    destilation_core::domain::TemplateMode::Preference => "Preference",
                    destilation_core::domain::TemplateMode::ToolTrace => "ToolTrace",
                    destilation_core::domain::TemplateMode::Custom => "Custom",
                },
            ),
            ("System Prompt", template.system_prompt.as_str()),
            ("User Prompt Pattern", template.user_prompt_pattern.as_str()),
        ];

        for (i, (label, value)) in fields.iter().enumerate() {
            let is_focused = state.template_edit_field_idx == i;
            let style = if is_focused {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };

            let block = Block::default()
                .title(*label)
                .borders(Borders::ALL)
                .border_style(style);

            let p = Paragraph::new(*value).block(block);
            f.render_widget(p, chunks[i]);
        }

        if let Some(preview) = &state.template_preview_output {
            let preview_area = centered_rect(80, 80, area);
            f.render_widget(Clear, preview_area);
            let block = Block::default()
                .title("Template Preview (Esc to Close)")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green));
            let p = Paragraph::new(preview.as_str()).block(block).wrap(Wrap { trim: true });
            f.render_widget(p, preview_area);
        }
    }
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

        let fields = [
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
                value.to_string()
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

fn render_log_event(ev: &LogEvent) -> Line<'_> {
    let ts = ev.ts.format("%H:%M:%S").to_string();
    let (lvl_str, lvl_style) = match ev.level {
        destilation_core::logging::LogLevel::Trace => ("TRC", Style::default().fg(Color::DarkGray)),
        destilation_core::logging::LogLevel::Debug => ("DBG", Style::default().fg(Color::Blue)),
        destilation_core::logging::LogLevel::Info => ("INF", Style::default().fg(Color::Green)),
        destilation_core::logging::LogLevel::Warn => ("WRN", Style::default().fg(Color::Yellow)),
        destilation_core::logging::LogLevel::Error => ("ERR", Style::default().fg(Color::Red)),
    };

    let mut spans = vec![
        Span::styled(format!("{} ", ts), Style::default().fg(Color::Gray)),
        Span::styled(format!("{} ", lvl_str), lvl_style),
        Span::raw(format!("{} ", ev.message)),
    ];

    if let Some(task_id) = ev.task_id.as_ref() {
        let id_short = if task_id.len() > 8 {
            &task_id[task_id.len() - 8..]
        } else {
            task_id
        };
        spans.push(Span::styled(format!("[{}] ", id_short), Style::default().fg(Color::Cyan)));
    }

    // Sort fields for consistent display
    let mut fields: Vec<_> = ev.fields.iter().collect();
    fields.sort_by_key(|(k, _)| *k);

    for (k, v) in fields {
        spans.push(Span::styled(format!("{}:", k), Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(format!("{} ", v), Style::default().fg(Color::White)));
    }

    Line::from(spans)
}

fn format_status_message(event: &LogEvent) -> String {
    let mut message = event.message.clone();
    if let Some(task_id) = &event.task_id {
        let short = if task_id.len() > 8 {
            &task_id[task_id.len() - 8..]
        } else {
            task_id
        };
        message = format!("{} [{}]", message, short);
    }
    if let Some(error) = event.fields.get("error") {
        message = format!("{}: {}", message, error);
    }
    message
}

fn in_flight_counts(state: &TuiState) -> (u64, u64, u64) {
    let mut generating = 0u64;
    let mut validating = 0u64;
    let mut queued = 0u64;
    for tasks in state.tasks.values() {
        for task in tasks {
            match task.state {
                destilation_core::domain::TaskState::Generating => generating += 1,
                destilation_core::domain::TaskState::Validating => validating += 1,
                destilation_core::domain::TaskState::Queued => queued += 1,
                _ => {}
            }
        }
    }
    (generating, validating, queued)
}

fn render_dashboard(f: &mut Frame, area: Rect, snap: &MetricsSnapshot, state: &mut TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
        .split(area);

    let right_chunks = if state.show_logs {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Percentage(50),
                    Constraint::Percentage(25),
                    Constraint::Percentage(25),
                ]
                .as_ref(),
            )
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)].as_ref())
            .split(chunks[1])
    };

    let active_providers = state.providers.iter().filter(|p| p.is_enabled()).count();
    let total_providers = state.providers.len();

    let (samples_per_min, tasks_per_min) = if let Some(start) = state.session_started_at {
        let elapsed = start.elapsed().as_secs_f64().max(0.001);
        let samples_delta = snap.samples_persisted.saturating_sub(state.samples_at_start);
        let tasks_delta = snap.tasks_persisted.saturating_sub(state.tasks_at_start);
        (samples_delta as f64 / elapsed * 60.0, tasks_delta as f64 / elapsed * 60.0)
    } else {
        (0.0, 0.0)
    };

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
        Line::from(vec![
            Span::raw("Samples Saved: "),
            Span::styled(
                format!("{}", snap.samples_persisted),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("Validator: "),
            Span::styled(
                format!("pass={} fail={}", snap.validator_pass, snap.validator_fail),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(vec![
            Span::raw("Throughput: "),
            Span::styled(
                format!("{:.1} samples/min", samples_per_min),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("Tasks: "),
            Span::styled(
                format!(
                    "enq={} started={} saved={} rej={}",
                    snap.tasks_enqueued,
                    snap.tasks_started,
                    snap.tasks_persisted,
                    snap.tasks_rejected
                ),
                Style::default().fg(Color::Blue),
            ),
        ]),
        Line::from(vec![
            Span::raw("Agent Rate: "),
            Span::styled(
                format!("{:.1} saved-tasks/min", tasks_per_min),
                Style::default().fg(Color::Cyan),
            ),
        ]),
    ];

    let stats =
        Paragraph::new(stats_text).block(Block::default().title("Execution").borders(Borders::ALL));
    f.render_widget(stats, chunks[0]);

    let items: Vec<ListItem> = state
        .jobs
        .iter()
        .map(|job| {
            let status_style = match job.status {
                destilation_core::domain::JobStatus::Completed => Style::default().fg(Color::Green),
                destilation_core::domain::JobStatus::Failed => Style::default().fg(Color::Red),
                destilation_core::domain::JobStatus::Running => Style::default().fg(Color::Yellow),
                destilation_core::domain::JobStatus::Paused => Style::default().fg(Color::Cyan),
                _ => Style::default(),
            };

            let tasks = state
                .tasks
                .get(&job.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let saved = tasks
                .iter()
                .filter(|t| t.state == destilation_core::domain::TaskState::Persisted)
                .count() as u64;
            let rejected = tasks
                .iter()
                .filter(|t| t.state == destilation_core::domain::TaskState::Rejected)
                .count() as u64;
            let received = tasks.iter().filter(|t| t.raw_response.is_some()).count() as u64;
            let target = job.config.target_samples;

            let job_start = job.started_at.unwrap_or(job.created_at);
            let job_elapsed =
                (chrono::Utc::now() - job_start).num_milliseconds().max(1) as f64 / 1000.0;
            let job_rate = saved as f64 / job_elapsed * 60.0;
            let remaining = target.saturating_sub(saved);
            let eta_secs = if job_rate > 0.0 {
                (remaining as f64) / (job_rate / 60.0)
            } else {
                f64::INFINITY
            };

            let eta_str = if eta_secs.is_finite() && eta_secs > 0.0 {
                format!("ETA {:.0}s", eta_secs)
            } else {
                "ETA --".to_string()
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "[{}] ",
                        if job.id.len() > 8 {
                            &job.id[job.id.len() - 8..]
                        } else {
                            &job.id
                        }
                    ),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(format!("{:?}", job.status), status_style),
                Span::raw(format!(
                    " {} | {}/{} saved | {} recv | {} rej | {:.1}/min | {}",
                    job.config.name, saved, target, received, rejected, job_rate, eta_str
                )),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().title("Jobs").borders(Borders::ALL))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, right_chunks[0], &mut state.job_list_state);

    let selected_id = state.selected_job_id_or_highlighted();
    let selected_job = selected_id
        .as_ref()
        .and_then(|id| state.jobs.iter().find(|j| &j.id == id));

    let mut detail_lines: Vec<Line> = Vec::new();
    if let Some(job) = selected_job {
        let tasks = state
            .tasks
            .get(&job.id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let received = tasks.iter().filter(|t| t.raw_response.is_some()).count() as u64;
        let saved = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Persisted)
            .count() as u64;
        let rejected = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Rejected)
            .count() as u64;
        let generating = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Generating)
            .count() as u64;
        let validating = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Validating)
            .count() as u64;
        let queued = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Queued)
            .count() as u64;

        let job_start = job.started_at.unwrap_or(job.created_at);
        let job_elapsed_secs =
            (chrono::Utc::now() - job_start).num_milliseconds().max(1) as f64 / 1000.0;
        let job_rate = saved as f64 / job_elapsed_secs * 60.0;
        let remaining = job.config.target_samples.saturating_sub(saved);
        let eta_secs = if job_rate > 0.0 {
            (remaining as f64) / (job_rate / 60.0)
        } else {
            f64::INFINITY
        };

        let eta_str = if eta_secs.is_finite() && eta_secs > 0.0 {
            format!("{:.0}s", eta_secs)
        } else {
            "--".to_string()
        };

        let last_error = tasks
            .iter()
            .filter(|t| t.state == destilation_core::domain::TaskState::Rejected)
            .filter_map(|t| t.validation_result.as_ref())
            .flat_map(|vr| vr.issues.iter().map(|i| i.code.clone()))
            .next()
            .unwrap_or_else(|| "none".to_string());

        detail_lines.push(Line::from(vec![
            Span::raw("Selected: "),
            Span::styled(
                &job.config.name,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        detail_lines.push(Line::from(vec![
            Span::raw("Status: "),
            Span::raw(format!("{:?}", job.status)),
            Span::raw(" | Started: "),
            Span::raw(job_start.to_rfc3339()),
        ]));
        detail_lines.push(Line::from(vec![
            Span::raw("Received: "),
            Span::styled(format!("{}", received), Style::default().fg(Color::Yellow)),
            Span::raw(" | Saved: "),
            Span::styled(
                format!("{}/{}", saved, job.config.target_samples),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" | Rejected: "),
            Span::styled(format!("{}", rejected), Style::default().fg(Color::Red)),
        ]));
        detail_lines.push(Line::from(vec![
            Span::raw("In-flight: "),
            Span::styled(
                format!("gen={} val={} queued={}", generating, validating, queued),
                Style::default().fg(Color::Yellow),
            ),
        ]));
        detail_lines.push(Line::from(vec![
            Span::raw("Throughput: "),
            Span::styled(
                format!("{:.2} samples/min", job_rate),
                Style::default().fg(Color::Magenta),
            ),
            Span::raw(" | ETA: "),
            Span::styled(eta_str, Style::default().fg(Color::Cyan)),
        ]));
        detail_lines.push(Line::from(vec![
            Span::raw("Last Error: "),
            Span::styled(last_error, Style::default().fg(Color::Red)),
        ]));
    } else {
        detail_lines.push(Line::from("No job selected"));
    }

    let detail = Paragraph::new(detail_lines)
        .block(Block::default().title("Selected Job").borders(Borders::ALL));
    f.render_widget(detail, right_chunks[1]);

    if state.show_logs {
        let selected_id = state.selected_job_id_or_highlighted();
        let title = if let Some(id) = selected_id.as_ref() {
            format!(
                "Logs ({})",
                if id.len() > 8 {
                    &id[id.len() - 8..]
                } else {
                    id
                }
            )
        } else {
            "Logs".to_string()
        };

        let max_lines = right_chunks[2].height.saturating_sub(2) as usize;
        let lines = if let Some(id) = selected_id.as_ref() {
            let buf = state.job_logs.get(id).map(|v| v.as_slice()).unwrap_or(&[]);
            let start = buf.len().saturating_sub(max_lines.max(1));
            buf[start..]
                .iter()
                .map(|ev| render_log_event(ev))
                .collect::<Vec<_>>()
        } else {
            let start = state.global_logs.len().saturating_sub(max_lines.max(1));
            state.global_logs[start..]
                .iter()
                .map(|ev| render_log_event(ev))
                .collect::<Vec<_>>()
        };

        let lines = if lines.is_empty() {
            vec![Line::from("No log events yet")]
        } else {
            lines
        };

        let logs = Paragraph::new(lines).block(Block::default().title(title).borders(Borders::ALL));
        f.render_widget(logs, right_chunks[2]);
    }
}

fn render_log_viewer(f: &mut Frame, area: Rect, state: &mut TuiState) {
    let (logs, title) = if let Some(job_id) = &state.selected_job_id {
        let job_logs = state.job_logs.get(job_id).cloned().unwrap_or_default();
        if job_logs.is_empty() {
            (
                state.global_logs.clone(),
                format!("Log Viewer - Global (Job {} has no logs) (Esc/F9 to exit)", job_id),
            )
        } else {
            (
                job_logs,
                format!("Log Viewer - Job {} (Esc/F9 to exit)", job_id),
            )
        }
    } else {
        (
            state.global_logs.clone(),
            "Log Viewer - Global (Esc/F9 to exit)".to_string(),
        )
    };

    let items: Vec<ListItem> = logs
        .iter()
        .rev()
        .map(|ev| {
            let line = render_log_event(ev);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut state.log_list_state);
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
                Constraint::Length(3), // Samples
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

    // Samples Input
    let samples_style = if state.job_create_field_idx == 2 {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let samples_input = Paragraph::new(state.job_create_samples.as_str()).block(
        Block::default()
            .title("Target Samples (e.g., 100)")
            .borders(Borders::ALL)
            .border_style(samples_style),
    );
    f.render_widget(samples_input, chunks[2]);

    // Template Selection
    let template_style = if state.job_create_field_idx == 3 {
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

    f.render_stateful_widget(list, chunks[3], &mut state.job_create_template_state);
}

fn render_provider_type_select(f: &mut Frame, area: Rect, state: &TuiState) {
    let area = centered_rect(60, 40, area);
    let items = vec![
        ListItem::new("OpenRouter"),
        ListItem::new("Ollama"),
        ListItem::new("Script"),
    ];
    let list = List::new(items)
        .block(
            Block::default()
                .title("Select Provider Type")
                .borders(Borders::ALL),
        )
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
            ("F6", "Logs"),
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
        View::LogViewer => vec![
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
        View::TemplateList => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", ""),
            ("F4", ""),
            ("F5", ""),
            ("F6", ""),
            ("F7", "New"),
            ("F8", "Delete"),
            ("F9", "Back"),
            ("F10", "Quit"),
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
            ("F5", "Test"),
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
        View::TemplateEdit => vec![
            ("F1", "Help"),
            ("F2", ""),
            ("F3", ""),
            ("F4", ""),
            ("F5", "Preview"),
            ("F6", ""),
            ("F7", ""),
            ("F8", ""),
            ("F9", "Cancel"),
            ("F10", "Save"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use destilation_core::storage::JobStore;

    fn mk_job(id: &str) -> destilation_core::domain::Job {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let store = destilation_core::storage::InMemoryJobStore::new();
        let config = destilation_core::domain::JobConfig {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            target_samples: 1,
            max_concurrency: 1,
            domains: vec![],
            template_id: "t".to_string(),
            reasoning_mode: destilation_core::domain::ReasoningMode::Simple,
            providers: vec![],
            validation: destilation_core::domain::JobValidationConfig {
                max_attempts: 1,
                validators: vec![],
                min_quality_score: None,
                fail_fast: false,
            },
            output: destilation_core::domain::JobOutputConfig {
                dataset_dir: ".".to_string(),
                shard_size: 1,
                compress: false,
                metadata: Default::default(),
            },
        };
        rt.block_on(async { store.create_job(config).await.unwrap() })
    }

    #[test]
    fn dashboard_selection_sets_selected_job_id() {
        let mut s = TuiState::new();
        s.jobs = vec![mk_job("a"), mk_job("b")];
        s.job_list_state.select(Some(0));
        s.sync_selected_job_id();
        assert_eq!(s.selected_job_id.as_deref(), Some("a"));
        s.next_job();
        assert_eq!(s.selected_job_id.as_deref(), Some("b"));
        s.previous_job();
        assert_eq!(s.selected_job_id.as_deref(), Some("a"));
    }
}

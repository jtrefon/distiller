use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use destilation_core::domain::{Job, Task};
use destilation_core::metrics::{Metrics, MetricsSnapshot};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{collections::HashMap, io, sync::{mpsc, Arc}, time::Duration};

pub enum TuiUpdate {
    Jobs(Vec<Job>),
    Tasks(JobId, Vec<Task>),
}

type JobId = String;

struct TuiState {
    jobs: Vec<Job>,
    tasks: HashMap<JobId, Vec<Task>>,
    job_list_state: ListState,
    selected_job_id: Option<JobId>,
    view: View,
}

#[derive(PartialEq)]
enum View {
    Dashboard,
    JobDetail,
}

impl TuiState {
    fn new() -> Self {
        Self {
            jobs: Vec::new(),
            tasks: HashMap::new(),
            job_list_state: ListState::default(),
            selected_job_id: None,
            view: View::Dashboard,
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
    state: TuiState,
}

impl Tui {
    pub fn new(metrics: Arc<dyn Metrics>, rx: mpsc::Receiver<TuiUpdate>) -> Self {
        Self {
            metrics,
            rx,
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
                }
            }

            let snap = self.metrics.snapshot();
            terminal
                .draw(|f| ui(f, &snap, &mut self.state))
                .map_err(|_| io::Error::other("draw failed"))?;

            if check_done() {
                // One last draw to show final state
                let snap = self.metrics.snapshot();
                terminal
                    .draw(|f| ui(f, &snap, &mut self.state))
                    .map_err(|_| io::Error::other("draw failed"))?;
                std::thread::sleep(Duration::from_millis(1000));
                return Ok(());
            }

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Down => self.state.next_job(),
                        KeyCode::Up => self.state.previous_job(),
                        KeyCode::Enter => self.state.enter_job(),
                        KeyCode::Esc => self.state.exit_detail(),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, snap: &MetricsSnapshot, state: &mut TuiState) {
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
    let title = Paragraph::new("Destilation Agentic Pipeline")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
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
    }

    // Footer
    let info_text = vec![
        Line::from(vec![
            Span::raw("Press "),
            Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to quit. "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to view job details. "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to back."),
        ]),
    ];
    let info =
        Paragraph::new(info_text).block(Block::default().title("Info").borders(Borders::ALL));
    f.render_widget(info, chunks[3]);
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

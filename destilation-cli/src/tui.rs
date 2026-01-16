use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use destilation_core::metrics::{Metrics, MetricsSnapshot};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame, Terminal,
};
use std::{io, sync::Arc, time::Duration};

pub struct Tui {
    metrics: Arc<dyn Metrics>,
}

impl Tui {
    pub fn new(metrics: Arc<dyn Metrics>) -> Self {
        Self { metrics }
    }

    pub fn run(&self, check_done: impl Fn() -> bool) -> io::Result<()> {
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
        &self,
        terminal: &mut Terminal<B>,
        check_done: impl Fn() -> bool,
    ) -> io::Result<()> {
        loop {
            let snap = self.metrics.snapshot();
            terminal
                .draw(|f| ui(f, &snap))
                .map_err(|_| io::Error::other("draw failed"))?;

            if check_done() {
                // One last draw to show final state
                let snap = self.metrics.snapshot();
                terminal
                    .draw(|f| ui(f, &snap))
                    .map_err(|_| io::Error::other("draw failed"))?;
                std::thread::sleep(Duration::from_millis(1000));
                return Ok(());
            }

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if let KeyCode::Char('q') = key.code {
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn ui(f: &mut Frame, snap: &MetricsSnapshot) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .split(f.area());

    let title = Paragraph::new("Destilation Agentic Pipeline")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(title, chunks[0]);

    let ratio = if snap.tasks_enqueued > 0 {
        (snap.tasks_persisted as f64 / snap.tasks_enqueued as f64).min(1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .block(Block::default().title("Progress").borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(ratio);
    f.render_widget(gauge, chunks[1]);

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
            Span::raw(" | Started: "),
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
            Span::raw(" | Rejected: "),
            Span::styled(
                format!("{}", snap.tasks_rejected),
                Style::default().fg(Color::Red),
            ),
        ]),
    ];
    let stats =
        Paragraph::new(stats_text).block(Block::default().title("Metrics").borders(Borders::ALL));
    f.render_widget(stats, chunks[2]);

    let info_text = vec![
        Line::from("Press 'q' to quit early."),
        Line::from("TUI updates every 100ms."),
    ];
    let info =
        Paragraph::new(info_text).block(Block::default().title("Info").borders(Borders::ALL));
    f.render_widget(info, chunks[3]);
}

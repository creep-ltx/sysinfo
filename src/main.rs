mod collect;
mod ui;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Tabs},
    Terminal,
};

use collect::{Sampler, StaticInfo, UpdatesInfo};

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
}

/// Restores the terminal on drop, so raw mode never outlives the program.
struct TermGuard;

impl TermGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // a panic mid-draw must not leave the shell in raw mode
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let _guard = TermGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let statics = StaticInfo::collect();
    let mut sampler = Sampler::new();
    let updates = Arc::new(Mutex::new(UpdatesInfo::default()));

    let bg = updates.clone();
    std::thread::spawn(move || loop {
        collect::check_updates(&bg);
        std::thread::sleep(Duration::from_secs(600));
    });

    let mut active_tab = 0usize;

    loop {
        sampler.sample();
        let content = match active_tab {
            0 => ui::system_tab(&statics, &mut sampler, &updates),
            1 => ui::cores_tab(&mut sampler),
            2 => ui::memory_tab(&mut sampler),
            3 => ui::gpu_tab(&statics, &mut sampler),
            4 => ui::net_tab(&statics, &mut sampler),
            5 => ui::sensors_tab(&statics, &mut sampler),
            _ => String::new(),
        };

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Length(3), Constraint::Min(0)])
                .split(f.size());

            let tabs_widget = Tabs::new(ui::TABS.to_vec())
                .block(Block::default().borders(Borders::ALL).title("Hardware Dashboard"))
                .select(active_tab)
                .style(Style::default().fg(Color::Cyan))
                .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

            let content_widget = Paragraph::new(content.as_str())
                .block(Block::default().borders(Borders::ALL).title("Details"))
                .style(Style::default().fg(Color::White));

            f.render_widget(tabs_widget, chunks[0]);
            f.render_widget(content_widget, chunks[1]);
        })?;

        if !event::poll(Duration::from_millis(500))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != event::KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('r') => {
                    let bg = updates.clone();
                    std::thread::spawn(move || collect::check_updates(&bg));
                }
                KeyCode::Left | KeyCode::Char('h') => active_tab = active_tab.saturating_sub(1),
                KeyCode::Right | KeyCode::Char('l') => {
                    active_tab = (active_tab + 1).min(ui::TABS.len() - 1)
                }
                KeyCode::Char(c @ '1'..='6') => active_tab = c as usize - '1' as usize,
                _ => {}
            }
        }
    }

    Ok(())
}

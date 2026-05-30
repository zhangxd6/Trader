//! Ratatui rendering of the three-panel layout.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table};
use ratatui::Frame;

use crate::tui::app::{App, AppStatus, LogLevel, RunMode};

/// Render the full UI for one frame.
pub fn render(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(f.area());

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[0]);

    render_strategy(f, app, top[0]);
    render_portfolio(f, app, top[1]);
    render_logs(f, app, outer[1]);
}

fn render_strategy(f: &mut Frame, app: &App, area: Rect) {
    let s = &app.strategy.structured;
    let mode_color = match app.mode {
        RunMode::Live => Color::Red,
        RunMode::DryRun => Color::Yellow,
        RunMode::Simulate => Color::Cyan,
    };

    let mut lines = vec![
        Line::from(Span::styled(
            app.strategy.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw("Mode: "),
            Span::styled(app.mode.label(), Style::default().fg(mode_color).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(Span::styled("HARD RULES", Style::default().fg(Color::Yellow))),
        Line::from(format!("  Stop-loss:      {:.1}%", s.stop_loss_pct)),
        Line::from(format!("  Take-profit:    {:.1}%", s.take_profit_pct)),
        Line::from(format!("  Max positions:  {}", s.max_positions)),
        Line::from(format!("  Min confidence: {:.2}", s.min_confidence)),
        Line::from("  Buy filters:"),
    ];
    if let Some(pe) = s.buy_filters.max_pe_ratio {
        lines.push(Line::from(format!("    P/E < {pe}")));
    }
    if let Some(h) = s.buy_filters.max_price_vs_52w_high_pct {
        lines.push(Line::from(format!("    Price < {h}% of 52w high")));
    }
    if let Some(v) = s.buy_filters.min_volume_ratio {
        lines.push(Line::from(format!("    Volume > {v}x avg")));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "JUDGMENT RULES",
        Style::default().fg(Color::Yellow),
    )));
    for (i, rule) in app.strategy.rules.iter().enumerate() {
        lines.push(Line::from(format!("  {}. {}", i + 1, rule)));
    }

    let block = Block::default().borders(Borders::ALL).title(" Strategy ");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_portfolio(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(3)])
        .split(area);

    // Summary header.
    let mut header_lines = Vec::new();
    if let Some(p) = &app.portfolio {
        let ret_color = if p.return_pct >= 0.0 { Color::Green } else { Color::Red };
        header_lines.push(Line::from(format!("Cash:   ${:>12.2}", p.cash)));
        header_lines.push(Line::from(format!("Equity: ${:>12.2}", p.market_value)));
        header_lines.push(Line::from(vec![
            Span::raw(format!("Total:  ${:>12.2}  ", p.total_value)),
            Span::styled(
                format!("{:+.2}%", p.return_pct),
                Style::default().fg(ret_color).add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        header_lines.push(Line::from(Span::styled(
            "awaiting first cycle...",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let status_span = match &app.status {
        AppStatus::Running => Span::styled("● Running", Style::default().fg(Color::Green)),
        AppStatus::Paused => Span::styled("● Paused", Style::default().fg(Color::Yellow)),
        AppStatus::Error(e) => Span::styled(format!("● Error: {e}"), Style::default().fg(Color::Red)),
    };
    let market = if app.market_open { "open" } else { "closed" };
    header_lines.push(Line::from(vec![
        status_span,
        Span::raw(format!("   market: {market}")),
    ]));

    let header_block = Block::default().borders(Borders::ALL).title(" Portfolio ");
    f.render_widget(Paragraph::new(header_lines).block(header_block), chunks[0]);

    // Positions table.
    let rows: Vec<Row> = app
        .portfolio
        .as_ref()
        .map(|p| {
            p.positions
                .iter()
                .map(|pos| {
                    let pnl_color = if pos.pnl_pct >= 0.0 { Color::Green } else { Color::Red };
                    Row::new(vec![
                        Cell::from(pos.symbol.clone()),
                        Cell::from(format!("{:.2}", pos.quantity)),
                        Cell::from(format!("${:.2}", pos.price)),
                        Cell::from(Span::styled(
                            format!("{:+.2}%", pos.pnl_pct),
                            Style::default().fg(pnl_color),
                        )),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();

    let widths = [
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(12),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["Symbol", "Qty", "Price", "P&L%"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(Block::default().borders(Borders::ALL).title(" Positions "));
    f.render_widget(table, chunks[1]);
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .log_buffer
        .iter()
        .map(|entry| {
            let color = match entry.level {
                LogLevel::Error => Color::Red,
                LogLevel::Order => Color::Green,
                LogLevel::Llm => Color::Cyan,
                LogLevel::Safety => Color::Yellow,
                LogLevel::Mcp => Color::Blue,
                LogLevel::Cycle => Color::Magenta,
                LogLevel::Info => Color::Gray,
            };
            let line = Line::from(vec![
                Span::styled(
                    entry.timestamp.format("%H:%M:%S ").to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:<7}", entry.level.tag()),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(entry.message.clone()),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = " Logs (last 20)   [q] quit  [p] pause ";
    let block = Block::default().borders(Borders::ALL).title(title);
    f.render_widget(List::new(items).block(block), area);
}

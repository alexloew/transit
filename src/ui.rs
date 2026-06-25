use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::{ActivePane, App, InputMode, UploadQueueItem, UploadStatus, ViewMode},
    local,
};

pub fn render(frame: &mut Frame<'_>, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(7),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, root[0], app);
    match app.view_mode {
        ViewMode::Browser => render_panes(frame, root[1], app),
        ViewMode::Transfers => render_transfers_page(frame, root[1], app),
    }
    render_logs(frame, root[2], app);
    render_help(frame, root[3], app);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let text = if app.input_mode == InputMode::Normal {
        vec![Line::from(vec![
            Span::styled(
                "transit",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw("local "),
            Span::styled(
                app.local_cwd.display().to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("  ->  "),
            Span::raw(if app.remote_host.is_empty() {
                "remote unset"
            } else {
                &app.remote_host
            }),
            Span::raw(":"),
            Span::styled(&app.remote_cwd, Style::default().fg(Color::Yellow)),
        ])]
    } else {
        vec![Line::from(vec![
            Span::styled(
                format!("{}: ", app.input_title()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&app.input_buffer),
            Span::styled(" ", Style::default().bg(Color::Cyan)),
        ])]
    };

    let block = Block::default().borders(Borders::ALL).title(" Transit ");
    frame.render_widget(Paragraph::new(text).block(block), area);
}

fn render_panes(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_local_pane(frame, chunks[0], app);
    render_remote_pane(frame, chunks[1], app);
}

fn render_local_pane(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = app
        .local_entries
        .iter()
        .map(|entry| {
            let marker = if entry.marked { "*" } else { " " };
            let kind = if entry.is_dir { "[D]" } else { "[F]" };
            let size = local::format_size(entry.size);
            let line = if size.is_empty() {
                format!("{marker} {kind} {:<6} {}", entry.file_type, entry.name)
            } else {
                format!(
                    "{marker} {kind} {:<6} {:<44} {size}",
                    entry.file_type, entry.name
                )
            };
            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let title = format!(" Local: {} ", app.local_cwd.display());
    render_list(
        frame,
        area,
        items,
        title,
        app.active_pane == ActivePane::Local,
        app.local_selected,
        app.local_entries.is_empty(),
    );
}

fn render_transfers_page(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(6)])
        .split(area);

    render_active_uploads(frame, chunks[0], app);
    render_upload_queue(frame, chunks[1], app);
}

fn render_active_uploads(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let active = app
        .transfer_queue
        .iter()
        .filter(|item| item.status == UploadStatus::Active)
        .collect::<Vec<_>>();
    let body = if active.is_empty() {
        if app.transferring {
            "Preparing next queued upload...".to_string()
        } else {
            "No active uploads.".to_string()
        }
    } else {
        active
            .iter()
            .enumerate()
            .map(|(offset, item)| format!("{}. {}", offset + 1, upload_item_line(item)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Active Uploads ");
    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_upload_queue(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let empty = app.transfer_queue.is_empty();
    let items = if empty {
        vec![ListItem::new("No uploads queued.")]
    } else {
        app.transfer_queue
            .iter()
            .enumerate()
            .map(|(offset, item)| {
                ListItem::new(format!("{:>2}. {}", offset + 1, upload_item_line(item)))
            })
            .collect::<Vec<_>>()
    };

    let queued = app
        .transfer_queue
        .iter()
        .filter(|item| item.status == UploadStatus::Queued)
        .count();
    let title = format!(" Queue (sequential, {queued} waiting) ");
    let block = Block::default().borders(Borders::ALL).title(title);
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    let mut state = ListState::default();
    if !empty {
        state.select(Some(
            app.transfer_selected.min(app.transfer_queue.len() - 1),
        ));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

fn upload_item_line(item: &UploadQueueItem) -> String {
    let progress = match (item.bytes_sent, item.bytes_total) {
        (Some(sent), Some(total)) if total > 0 => {
            let sent = sent.min(total);
            let percent = sent as f64 / total as f64 * 100.0;
            format!(
                "{percent:>5.1}% {} / {}",
                local::format_size(Some(sent)),
                local::format_size(Some(total))
            )
        }
        (_, Some(total)) if total > 0 => format!("{} total", local::format_size(Some(total))),
        _ => "size unknown".to_string(),
    };
    let message = item
        .message
        .as_ref()
        .map(|message| format!(" - {message}"))
        .unwrap_or_default();

    format!(
        "[{}] {} ({progress}, {}s){message}",
        upload_status_label(item.status),
        item.name,
        item.elapsed_secs
    )
}

fn upload_status_label(status: UploadStatus) -> &'static str {
    match status {
        UploadStatus::Queued => "queued",
        UploadStatus::Active => "active",
        UploadStatus::Done => "done",
        UploadStatus::Failed => "failed",
        UploadStatus::Canceled => "canceled",
    }
}

fn render_remote_pane(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = app
        .remote_entries
        .iter()
        .map(|entry| {
            let kind = if entry.is_dir { "[D]" } else { "[F]" };
            ListItem::new(format!("  {kind} {}", entry.name))
        })
        .collect::<Vec<_>>();

    let host = if app.remote_host.is_empty() {
        "unset"
    } else {
        &app.remote_host
    };
    let title = format!(" Remote: {host}:{} ", app.remote_cwd);
    render_list(
        frame,
        area,
        items,
        title,
        app.active_pane == ActivePane::Remote,
        app.remote_selected,
        app.remote_entries.is_empty(),
    );
}

fn render_list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    title: String,
    active: bool,
    selected: usize,
    empty: bool,
) {
    let border_style = if active {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    let mut state = ListState::default();
    if !empty {
        state.select(Some(selected));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_logs(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = if app.transferring {
        "transfer running"
    } else if app.remote_loading {
        "remote loading"
    } else {
        "idle"
    };
    let mut lines = Vec::new();
    if let Some(transfer_status) = &app.transfer_status {
        lines.push(transfer_status.clone());
    }
    lines.extend(app.logs.iter().cloned());
    let body = if lines.is_empty() {
        "No activity yet.".to_string()
    } else {
        lines.join("\n")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Activity ({status}) "));
    frame.render_widget(
        Paragraph::new(body).block(block).wrap(Wrap { trim: true }),
        area,
    );
}

fn render_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let help = if app.input_mode != InputMode::Normal {
        "enter accept | esc cancel | type to edit"
    } else if app.view_mode == ViewMode::Transfers {
        "j/k arrows move | x cancel queued | t browser | q quit"
    } else {
        "tab pane | j/k arrows move | enter open | h/backspace parent | space mark | u upload | t transfers | r refresh | o host | g path | q quit"
    };

    frame.render_widget(Paragraph::new(help), area);
}

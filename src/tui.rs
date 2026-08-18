use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use paddock::{
    items_in_chain, load_or_init, pull_all, spawn_fs_watch, Config, InboxConfig, Item, Paths, Store,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use std::io::{self, stdout};
use std::time::Duration;

enum Pane {
    Inboxes,
    Items,
}

enum Mode {
    List,
    Read,
    Label,
    Help,
}

struct App {
    paths: Paths,
    store: Store,
    config: Config,
    tree: Vec<(Vec<String>, usize, InboxConfig)>, // path, depth, cfg
    tree_state: ListState,
    items: Vec<Item>,
    item_state: ListState,
    pane: Pane,
    mode: Mode,
    label_buf: String,
    status: String,
    last_gen: u64,
}

impl App {
    fn new(paths: Paths, store: Store, config: Config) -> Self {
        let mut app = Self {
            paths,
            store,
            config,
            tree: Vec::new(),
            tree_state: ListState::default().with_selected(Some(0)),
            items: Vec::new(),
            item_state: ListState::default(),
            pane: Pane::Items,
            mode: Mode::List,
            label_buf: String::new(),
            status: "r pull   space read   l label   ? help   q quit".into(),
            last_gen: paddock::engine::gen(),
        };
        app.reload_tree();
        app.reload_items();
        app
    }

    fn selected_path(&self) -> Vec<String> {
        self.tree_state
            .selected()
            .and_then(|i| self.tree.get(i))
            .map(|(p, _, _)| p.clone())
            .unwrap_or_default()
    }

    fn chain(&self) -> Vec<&InboxConfig> {
        let path = self.selected_path();
        let refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        self.config.find_chain(&refs).unwrap_or_default()
    }

    fn reload_tree(&mut self) {
        let flat = self.config.flatten();
        self.tree = flat
            .into_iter()
            .map(|n| (n.path, n.depth, n.inbox))
            .collect();
        if self.tree.is_empty() {
            self.tree_state.select(None);
        } else if self.tree_state.selected().map(|i| i >= self.tree.len()).unwrap_or(true) {
            self.tree_state.select(Some(0));
        }
    }

    fn reload_items(&mut self) {
        let chain = self.chain();
        self.items = items_in_chain(&self.store, &chain).unwrap_or_default();
        if self.items.is_empty() {
            self.item_state.select(None);
        } else {
            let i = self.item_state.selected().unwrap_or(0);
            self.item_state.select(Some(i.min(self.items.len() - 1)));
        }
    }

    fn counts(&self, path: &[String]) -> (usize, usize) {
        let refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let chain = match self.config.find_chain(&refs) {
            Some(c) => c,
            None => return (0, 0),
        };
        let items = items_in_chain(&self.store, &chain).unwrap_or_default();
        let unread = items.iter().filter(|i| !i.read).count();
        (unread, items.len())
    }

    fn move_tree(&mut self, delta: isize) {
        if self.tree.is_empty() {
            return;
        }
        let i = self.tree_state.selected().unwrap_or(0) as isize;
        let n = self.tree.len() as isize;
        let i = (i + delta).rem_euclid(n) as usize;
        self.tree_state.select(Some(i));
        self.reload_items();
    }

    fn move_items(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let i = self.item_state.selected().unwrap_or(0) as isize;
        let n = self.items.len() as isize;
        let i = (i + delta).rem_euclid(n) as usize;
        self.item_state.select(Some(i));
    }

    fn current_item_id(&self) -> Option<i64> {
        self.item_state
            .selected()
            .and_then(|i| self.items.get(i))
            .map(|i| i.id)
    }

    fn pull(&mut self) {
        match Config::load(&self.paths.config_file) {
            Ok(c) => self.config = c,
            Err(e) => {
                self.status = format!("config: {e}");
                return;
            }
        }
        self.reload_tree();
        match pull_all(&self.store, &self.config) {
            Ok(n) => self.status = format!("admitted {n}"),
            Err(e) => self.status = format!("pull: {e}"),
        }
        self.reload_items();
    }

    fn toggle_read(&mut self) {
        let Some(id) = self.current_item_id() else { return };
        match self.store.toggle_read(id) {
            Ok(read) => {
                self.status = if read { "read".into() } else { "unread".into() };
                self.reload_items();
            }
            Err(e) => self.status = format!("{e}"),
        }
    }

    fn submit_label(&mut self) {
        let label = self.label_buf.trim().to_string();
        self.label_buf.clear();
        self.mode = Mode::List;
        if label.is_empty() {
            return;
        }
        let Some(id) = self.current_item_id() else { return };
        match self.store.toggle_label(id, &label) {
            Ok(on) => {
                self.status = if on {
                    format!("+{label}")
                } else {
                    format!("-{label}")
                };
                self.reload_items();
            }
            Err(e) => self.status = format!("{e}"),
        }
    }

    fn poll_watch(&mut self) {
        let g = paddock::engine::gen();
        if g != self.last_gen {
            self.last_gen = g;
            self.reload_items();
            self.status = "admitted (watch)".into();
        }
    }
}

pub fn run(paths: Paths) -> Result<()> {
    let (config, store) = load_or_init(&paths)?;
    let _watch = spawn_fs_watch(store.clone(), paths.clone()).ok();
    let mut app = App::new(paths, store, config);

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        hook(info);
    }));

    let result = loop_ui(&mut terminal, &mut app);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn loop_ui(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(app, key.code) {
                    break;
                }
            }
        }
        app.poll_watch();
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) -> bool {
    match app.mode {
        Mode::Help => {
            app.mode = Mode::List;
            return false;
        }
        Mode::Read => {
            match code {
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Enter => {
                    app.mode = Mode::List;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char(' ') => app.toggle_read(),
                KeyCode::Char('j') | KeyCode::Down => {
                    app.move_items(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.move_items(-1);
                }
                _ => {}
            }
            return false;
        }
        Mode::Label => {
            match code {
                KeyCode::Esc => {
                    app.label_buf.clear();
                    app.mode = Mode::List;
                }
                KeyCode::Enter => app.submit_label(),
                KeyCode::Backspace => {
                    app.label_buf.pop();
                }
                KeyCode::Char(c) if !c.is_control() => app.label_buf.push(c),
                _ => {}
            }
            return false;
        }
        Mode::List => {}
    }

    match code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char('r') => app.pull(),
        KeyCode::Char(' ') => app.toggle_read(),
        KeyCode::Char('l') => {
            if app.current_item_id().is_some() {
                app.mode = Mode::Label;
                app.label_buf.clear();
            }
        }
        KeyCode::Tab => {
            app.pane = match app.pane {
                Pane::Inboxes => Pane::Items,
                Pane::Items => Pane::Inboxes,
            };
        }
        KeyCode::Enter => {
            if matches!(app.pane, Pane::Inboxes) {
                app.pane = Pane::Items;
            } else if app.current_item_id().is_some() {
                app.mode = Mode::Read;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => match app.pane {
            Pane::Inboxes => app.move_tree(1),
            Pane::Items => app.move_items(1),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.pane {
            Pane::Inboxes => app.move_tree(-1),
            Pane::Items => app.move_items(-1),
        },
        KeyCode::Char('g') => match app.pane {
            Pane::Inboxes => {
                if !app.tree.is_empty() {
                    app.tree_state.select(Some(0));
                    app.reload_items();
                }
            }
            Pane::Items => {
                if !app.items.is_empty() {
                    app.item_state.select(Some(0));
                }
            }
        },
        KeyCode::Char('G') => match app.pane {
            Pane::Inboxes => {
                if !app.tree.is_empty() {
                    app.tree_state.select(Some(app.tree.len() - 1));
                    app.reload_items();
                }
            }
            Pane::Items => {
                if !app.items.is_empty() {
                    app.item_state.select(Some(app.items.len() - 1));
                }
            }
        },
        KeyCode::Left | KeyCode::Char('h') => app.pane = Pane::Inboxes,
        KeyCode::Right => app.pane = Pane::Items,
        _ => {}
    }
    false
}

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(20)])
        .split(chunks[0]);

    draw_tree(f, app, body[0]);
    match app.mode {
        Mode::Read => draw_read(f, app, body[1]),
        _ => draw_items(f, app, body[1]),
    }
    draw_status(f, app, chunks[1]);

    if matches!(app.mode, Mode::Help) {
        draw_help(f);
    }
    if matches!(app.mode, Mode::Label) {
        draw_label(f, app);
    }
}

fn sel_style(active: bool) -> Style {
    if active {
        Style::default()
            .bg(Color::Rgb(40, 40, 40))
            .fg(Color::Rgb(230, 230, 220))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(200, 200, 190))
    }
}

fn draw_tree(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let active = matches!(app.pane, Pane::Inboxes) && matches!(app.mode, Mode::List);
    let border = if active { Color::Rgb(180, 160, 80) } else { Color::Rgb(50, 50, 50) };
    let items: Vec<ListItem> = if app.tree.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  (no inboxes)",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.tree
            .iter()
            .map(|(path, depth, ib)| {
                let (unread, total) = app.counts(path);
                let pad = "  ".repeat(*depth);
                let name = &ib.name;
                Line::from(vec![
                    Span::raw(format!("{pad}{name}")),
                    Span::styled(
                        format!("  {unread}/{total}"),
                        Style::default().fg(Color::Rgb(110, 110, 110)),
                    ),
                ])
                .into()
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" inbox ")
                .border_style(Style::default().fg(border)),
        )
        .highlight_style(sel_style(active));
    f.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_items(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let active = matches!(app.pane, Pane::Items) && matches!(app.mode, Mode::List);
    let border = if active { Color::Rgb(180, 160, 80) } else { Color::Rgb(50, 50, 50) };
    let title = {
        let p = app.selected_path();
        if p.is_empty() {
            " items ".into()
        } else {
            format!(" {} ", p.join("/"))
        }
    };
    let items: Vec<ListItem> = if app.items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  empty",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.items
            .iter()
            .map(|it| {
                let mark = if it.read { " " } else { "*" };
                let when = short_time(&it.created_at);
                let style = if it.read {
                    Style::default().fg(Color::Rgb(90, 90, 90))
                } else {
                    Style::default().fg(Color::Rgb(220, 220, 210))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{mark} "), style),
                    Span::styled(trunc(&it.title, 42), style.add_modifier(Modifier::BOLD)),
                    Span::styled(
                        format!("  {}  {when}", it.source_id),
                        Style::default().fg(Color::Rgb(100, 100, 100)),
                    ),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(border)),
        )
        .highlight_style(sel_style(active));
    f.render_stateful_widget(list, area, &mut app.item_state);
}

fn draw_read(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let Some(i) = app.item_state.selected() else {
        f.render_widget(
            Paragraph::new("empty").block(Block::default().borders(Borders::ALL).title(" item ")),
            area,
        );
        return;
    };
    let Some(it) = app.items.get(i) else { return };
    let labels = if it.labels.is_empty() {
        "—".into()
    } else {
        it.labels.join(" ")
    };
    let head = format!(
        "{}\n{}  {}  {}\nlabels  {labels}\n",
        it.title,
        it.source_id,
        short_time(&it.created_at),
        it.href.as_deref().unwrap_or(""),
    );
    let text = format!("{head}\n{}", it.body);
    let p = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" item  esc back ")
                .border_style(Style::default().fg(Color::Rgb(180, 160, 80))),
        );
    f.render_widget(p, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let msg = match app.mode {
        Mode::Label => format!("label: {}_", app.label_buf),
        _ => app.status.clone(),
    };
    f.render_widget(
        Paragraph::new(msg).style(Style::default().fg(Color::Rgb(140, 140, 130)).bg(Color::Rgb(10, 10, 10))),
        area,
    );
}

fn draw_help(f: &mut ratatui::Frame) {
    let area = centered(f.area(), 50, 16);
    let text = "\
 j/k  ↑↓     move
 tab  h/l    pane
 enter       read
 space       toggle read
 l           toggle label
 r           pull
 g / G       top / bottom
 ?           help
 q           quit
 esc         close
";
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" keys ")
                .border_style(Style::default().fg(Color::Rgb(180, 160, 80))),
        ),
        area,
    );
}

fn draw_label(f: &mut ratatui::Frame, app: &App) {
    let area = centered(f.area(), 40, 3);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(format!(" {}", app.label_buf)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" label (enter toggle) ")
                .border_style(Style::default().fg(Color::Rgb(180, 160, 80))),
        ),
        area,
    );
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn short_time(rfc: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| rfc.chars().take(16).collect())
}

fn trunc(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= n {
        s.to_string()
    } else {
        format!("{}…", c.into_iter().take(n.saturating_sub(1)).collect::<String>())
    }
}

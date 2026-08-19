use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use paddock::cmd::{run_verb, VerbCtx};
use paddock::keys::{feed, parse_colon, Feed, KeySeq, Verb, HELP};
use paddock::theme::{load_theme, Theme};
use paddock::{
    collapse_threads, display_width, filter_visible_inboxes, items_in_chain, load_or_init,
    open_inbox_path, pad_width, row_who_text, source_label, spawn_fs_watch, trunc_width,
    view_prefix, Config, InboxConfig, Item, ListRow, Paths, Store, IDLE_HINT,
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
    Search,
    Command,
    Compose,
}

enum ComposeField {
    Title,
    Body,
}

struct App {
    paths: Paths,
    store: Store,
    config: Config,
    theme: Theme,
    tree: Vec<(Vec<String>, usize, InboxConfig)>,
    tree_state: ListState,
    items: Vec<ListRow>,
    item_state: ListState,
    pane: Pane,
    mode: Mode,
    from_read: bool,
    label_buf: String,
    search_buf: String,
    search_hits: Vec<usize>,
    search_at: usize,
    cmd_buf: String,
    status: String,
    last_gen: u64,
    keys: KeySeq,
    unread_only: bool,
    list_height: usize,
    from_compose: bool,
    compose_reply_to: Option<i64>,
    compose_title: String,
    compose_body: String,
    compose_field: ComposeField,
}

impl App {
    fn new(paths: Paths, store: Store, config: Config) -> Self {
        let theme = load_theme(&config, &paths);
        let mut app = Self {
            paths,
            store,
            config,
            theme,
            tree: Vec::new(),
            tree_state: ListState::default().with_selected(Some(0)),
            items: Vec::new(),
            item_state: ListState::default(),
            pane: Pane::Items,
            mode: Mode::List,
            from_read: false,
            label_buf: String::new(),
            search_buf: String::new(),
            search_hits: Vec::new(),
            search_at: 0,
            cmd_buf: String::new(),
            status: IDLE_HINT.into(),
            last_gen: paddock::engine::gen(),
            keys: KeySeq::default(),
            unread_only: true,
            list_height: 12,
            from_compose: false,
            compose_reply_to: None,
            compose_title: String::new(),
            compose_body: String::new(),
            compose_field: ComposeField::Title,
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
        let keep = self.selected_path();
        let open = if keep.is_empty() || !flat.iter().any(|n| n.path == keep) {
            open_inbox_path(&flat, |p| self.counts(p).1)
        } else {
            keep
        };
        let visible = filter_visible_inboxes(&flat, &open, |p| self.counts(p).1);
        self.tree = visible
            .into_iter()
            .map(|n| (n.path, n.depth, n.inbox))
            .collect();
        if self.tree.is_empty() {
            self.tree_state.select(None);
        } else {
            let idx = self
                .tree
                .iter()
                .position(|(p, _, _)| *p == open)
                .unwrap_or(0);
            self.tree_state.select(Some(idx));
        }
    }

    fn reload_items(&mut self) {
        self.reload_tree();
        let chain = self.chain();
        let mut items = items_in_chain(&self.store, &chain).unwrap_or_default();
        if self.unread_only {
            items.retain(|i| !i.read);
        }
        self.items = collapse_threads(items);
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
        let mut filter = paddock::filter_for_chain(&chain);
        let total = self.store.count_filtered(&filter).unwrap_or(0);
        filter.unread_only = true;
        let unread = self.store.count_filtered(&filter).unwrap_or(0);
        (unread, total)
    }

    fn move_tree(&mut self, delta: isize) {
        if self.tree.is_empty() {
            return;
        }
        let i = self.tree_state.selected().unwrap_or(0) as isize;
        let n = self.tree.len() as isize;
        let i = (i + delta).clamp(0, n - 1) as usize;
        self.tree_state.select(Some(i));
        self.reload_items();
    }

    fn move_items(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let i = self.item_state.selected().unwrap_or(0) as isize;
        let n = self.items.len() as isize;
        let i = (i + delta).clamp(0, n - 1) as usize;
        self.item_state.select(Some(i));
    }

    fn move_current(&mut self, delta: isize) {
        match self.pane {
            Pane::Inboxes => self.move_tree(delta),
            Pane::Items => self.move_items(delta),
        }
    }

    fn jump_current(&mut self, index: usize) {
        match self.pane {
            Pane::Inboxes => {
                if !self.tree.is_empty() {
                    self.tree_state
                        .select(Some(index.min(self.tree.len() - 1)));
                    self.reload_items();
                }
            }
            Pane::Items => {
                if !self.items.is_empty() {
                    self.item_state
                        .select(Some(index.min(self.items.len() - 1)));
                }
            }
        }
    }

    fn page(&self) -> isize {
        self.list_height.max(1) as isize
    }

    fn current_item_id(&self) -> Option<i64> {
        self.item_state
            .selected()
            .and_then(|i| self.items.get(i))
            .map(|r| r.item.id)
    }

    fn ctx(&self) -> VerbCtx {
        VerbCtx {
            item_id: self.current_item_id(),
            inbox_path: self.selected_path(),
            unread_only: self.unread_only,
        }
    }

    fn apply_outcome(&mut self, out: paddock::Outcome) {
        if !out.status.is_empty() {
            self.status = out.status;
        }
        if let Some(u) = out.unread_only {
            self.unread_only = u;
        }
        if out.reload_config {
            if let Ok(c) = Config::load(&self.paths.config_file) {
                self.config = c;
                self.reload_tree();
            }
        }
        if let Some(name) = out.theme_name {
            self.theme = paddock::theme::load_named(&name, &self.paths);
        }
        if out.overlay.is_some() {
            self.mode = Mode::Help;
        }
        if out.open_compose {
            self.from_read = matches!(self.mode, Mode::Read);
            self.compose_reply_to = out.reply_to;
            self.compose_title = out
                .reply_to
                .and_then(|id| self.store.get(id).ok())
                .map(|p| paddock::reply_title(&p))
                .unwrap_or_default();
            self.compose_body.clear();
            self.compose_field = ComposeField::Title;
            self.mode = Mode::Compose;
            self.status = match out.reply_to {
                Some(id) => format!("reply to #{id}"),
                None => "compose".into(),
            };
        }
        self.reload_items();
    }

    fn exec(&mut self, verb: &Verb) -> bool {
        match run_verb(&self.store, &self.config, &self.paths, &self.ctx(), verb) {
            Ok(out) => {
                let quit = out.quit;
                self.apply_outcome(out);
                quit
            }
            Err(e) => {
                self.status = format!("{e}");
                false
            }
        }
    }

    fn submit_label(&mut self) {
        let label = self.label_buf.trim().to_string();
        self.label_buf.clear();
        self.mode = Mode::List;
        if label.is_empty() {
            return;
        }
        self.exec(&Verb::Relabel { label });
    }

    fn run_colon(&mut self) -> bool {
        let raw = self.cmd_buf.trim().to_string();
        self.cmd_buf.clear();
        let from_compose = self.from_compose;
        self.mode = if from_compose {
            Mode::Compose
        } else if self.from_read {
            Mode::Read
        } else {
            Mode::List
        };
        self.from_read = false;
        self.from_compose = false;
        if raw.is_empty() {
            return false;
        }
        match parse_colon(&raw) {
            Ok(verb) => {
                if from_compose && matches!(verb, Verb::Send { .. }) {
                    return send_compose(self);
                }
                if verb.is_local() && !matches!(verb, Verb::Help | Verb::Quit) {
                    return apply_local(self, verb);
                }
                self.exec(&verb)
            }
            Err(e) => {
                self.status = e;
                false
            }
        }
    }

    fn commit_search(&mut self) {
        self.rebuild_hits();
        self.mode = if self.from_read {
            Mode::Read
        } else {
            Mode::List
        };
        self.from_read = false;
        if self.search_hits.is_empty() {
            self.status = format!("/{0}  0", self.search_buf);
            return;
        }
        self.search_at = 0;
        if let Some(&i) = self.search_hits.first() {
            self.item_state.select(Some(i));
            self.pane = Pane::Items;
        }
        self.status = format!(
            "/{}  {}/{}",
            self.search_buf,
            self.search_at + 1,
            self.search_hits.len()
        );
    }

    fn rebuild_hits(&mut self) {
        let q = self.search_buf.to_lowercase();
        self.search_hits = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                let it = &row.item;
                it.title.to_lowercase().contains(&q) || it.body.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
    }

    fn search_step(&mut self, dir: isize) {
        if self.search_buf.is_empty() {
            return;
        }
        self.rebuild_hits();
        if self.search_hits.is_empty() {
            self.status = format!("/{}  0", self.search_buf);
            return;
        }
        let cur = self.item_state.selected().unwrap_or(0);
        let next = if dir > 0 {
            self.search_hits
                .iter()
                .copied()
                .find(|&i| i > cur)
                .unwrap_or(self.search_hits[0])
        } else {
            self.search_hits
                .iter()
                .copied()
                .rev()
                .find(|&i| i < cur)
                .unwrap_or(*self.search_hits.last().unwrap())
        };
        self.search_at = self
            .search_hits
            .iter()
            .position(|&i| i == next)
            .unwrap_or(0);
        self.item_state.select(Some(next));
        self.pane = Pane::Items;
        self.status = format!(
            "/{}  {}/{}",
            self.search_buf,
            self.search_at + 1,
            self.search_hits.len()
        );
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
                if handle_key(app, key) {
                    break;
                }
            }
        }
        app.poll_watch();
    }
    Ok(())
}

fn key_part(key: &KeyEvent) -> Option<String> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) if ctrl => Some(format!("C-{}", c.to_ascii_lowercase())),
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("Enter".into()),
        KeyCode::Esc => Some("Esc".into()),
        KeyCode::Tab => Some("Tab".into()),
        KeyCode::Backspace => Some("Backspace".into()),
        KeyCode::Up => Some("Up".into()),
        KeyCode::Down => Some("Down".into()),
        KeyCode::Left => Some("Left".into()),
        KeyCode::Right => Some("Right".into()),
        _ => None,
    }
}

fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    match app.mode {
        Mode::Help => {
            if key_part(&key).as_deref() == Some("Esc") || key.code == KeyCode::Char('q') {
                app.mode = Mode::List;
            } else if key_part(&key).as_deref() != Some("q") {
                app.mode = Mode::List;
            }
            return false;
        }
        Mode::Label => return handle_line(app, key, LineKind::Label),
        Mode::Search => return handle_line(app, key, LineKind::Search),
        Mode::Command => return handle_line(app, key, LineKind::Command),
        Mode::Compose => return handle_compose(app, key),
        Mode::Read | Mode::List => {}
    }

    let Some(part) = key_part(&key) else {
        return false;
    };
    if part == "Esc" && !app.keys.pending.is_empty() {
        app.keys.clear();
        return false;
    }
    match feed(&mut app.keys, &part) {
        Feed::Pending => false,
        Feed::None => false,
        Feed::Verb(v) => apply_verb(app, v),
    }
}

enum LineKind {
    Label,
    Search,
    Command,
}

fn handle_line(app: &mut App, key: KeyEvent, kind: LineKind) -> bool {
    match key.code {
        KeyCode::Esc => {
            match kind {
                LineKind::Label => app.label_buf.clear(),
                LineKind::Search => app.search_buf.clear(),
                LineKind::Command => app.cmd_buf.clear(),
            }
            app.mode = if app.from_compose {
                Mode::Compose
            } else if app.from_read {
                Mode::Read
            } else {
                Mode::List
            };
            app.from_read = false;
            app.from_compose = false;
        }
        KeyCode::Enter => match kind {
            LineKind::Label => app.submit_label(),
            LineKind::Search => app.commit_search(),
            LineKind::Command => return app.run_colon(),
        },
        KeyCode::Backspace => {
            match kind {
                LineKind::Label => {
                    app.label_buf.pop();
                }
                LineKind::Search => {
                    app.search_buf.pop();
                }
                LineKind::Command => {
                    app.cmd_buf.pop();
                }
            };
        }
        KeyCode::Char(c) if !c.is_control() => match kind {
            LineKind::Label => app.label_buf.push(c),
            LineKind::Search => app.search_buf.push(c),
            LineKind::Command => app.cmd_buf.push(c),
        },
        _ => {}
    }
    false
}

fn read_ok(v: &Verb) -> bool {
    matches!(
        v,
        Verb::Down
            | Verb::Up
            | Verb::Escape
            | Verb::PaneTree
            | Verb::Command
            | Verb::Quit
            | Verb::Help
            | Verb::ToggleRead
            | Verb::Eat
            | Verb::Forget
            | Verb::Unread
            | Verb::Bury
            | Verb::Todo
            | Verb::Again
            | Verb::Why
            | Verb::Yank
            | Verb::Open
            | Verb::Only
            | Verb::Spill
            | Verb::Pull
            | Verb::Theme { .. }
            | Verb::Themes
            | Verb::Which
            | Verb::Db
            | Verb::New { .. }
            | Verb::Relabel { .. }
            | Verb::Reply
            | Verb::Compose
            | Verb::Send { .. }
    )
}

fn apply_verb(app: &mut App, v: Verb) -> bool {
    if matches!(app.mode, Mode::Read) && !read_ok(&v) {
        return false;
    }
    if v.is_local() && !matches!(v, Verb::Help | Verb::Quit) {
        return apply_local(app, v);
    }
    app.exec(&v)
}

fn apply_local(app: &mut App, v: Verb) -> bool {
    match v {
        Verb::Down => {
            if matches!(app.mode, Mode::Read) {
                app.move_items(1);
            } else {
                app.move_current(1);
            }
        }
        Verb::Up => {
            if matches!(app.mode, Mode::Read) {
                app.move_items(-1);
            } else {
                app.move_current(-1);
            }
        }
        Verb::Top => app.jump_current(0),
        Verb::Bottom => {
            let n = match app.pane {
                Pane::Inboxes => app.tree.len(),
                Pane::Items => app.items.len(),
            };
            if n > 0 {
                app.jump_current(n - 1);
            }
        }
        Verb::HalfPageDown => app.move_current(app.page() / 2),
        Verb::HalfPageUp => app.move_current(-(app.page() / 2)),
        Verb::PageDown => app.move_current(app.page()),
        Verb::PageUp => app.move_current(-app.page()),
        Verb::PaneTree | Verb::Escape => {
            if matches!(app.mode, Mode::Read) {
                app.mode = Mode::List;
            } else {
                app.pane = Pane::Inboxes;
            }
        }
        Verb::PaneItems => app.pane = Pane::Items,
        Verb::SwapPane => {
            app.pane = match app.pane {
                Pane::Inboxes => Pane::Items,
                Pane::Items => Pane::Inboxes,
            };
        }
        Verb::OpenRead => {
            if matches!(app.pane, Pane::Inboxes) {
                app.pane = Pane::Items;
            } else if app.current_item_id().is_some() {
                app.mode = Mode::Read;
            }
        }
        Verb::NextInbox => app.move_tree(1),
        Verb::PrevInbox => app.move_tree(-1),
        Verb::Search => {
            app.from_read = matches!(app.mode, Mode::Read);
            app.mode = Mode::Search;
            app.search_buf.clear();
        }
        Verb::SearchNext => app.search_step(1),
        Verb::SearchPrev => app.search_step(-1),
        Verb::Command => {
            app.from_read = matches!(app.mode, Mode::Read);
            app.mode = Mode::Command;
            app.cmd_buf.clear();
        }
        Verb::LabelPrompt => {
            if app.current_item_id().is_some() && matches!(app.mode, Mode::List) {
                app.mode = Mode::Label;
                app.label_buf.clear();
            }
        }
        Verb::Help => {
            app.mode = Mode::Help;
        }
        Verb::Quit => return true,
        _ => {}
    }
    false
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

fn sel_style(theme: &Theme, active: bool) -> Style {
    if active {
        Style::default()
            .bg(rgb(theme.c_select()))
            .fg(rgb(theme.c_unread()))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(rgb(theme.c_fg()))
    }
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

    if app.list_height == 0 {
        app.list_height = body[1].height.saturating_sub(2) as usize;
    }

    draw_tree(f, app, body[0]);
    match app.mode {
        Mode::Read => draw_read(f, app, body[1]),
        Mode::Compose => draw_compose(f, app, body[1]),
        _ => draw_items(f, app, body[1]),
    }
    draw_status(f, app, chunks[1]);

    if matches!(app.mode, Mode::Help) {
        draw_help(f, app);
    }
    if matches!(app.mode, Mode::Label) {
        draw_label(f, app);
    }
}

fn draw_tree(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let th = &app.theme;
    let active = matches!(app.pane, Pane::Inboxes) && matches!(app.mode, Mode::List);
    let border = if active {
        rgb(th.c_accent())
    } else {
        rgb(th.c_border())
    };
    let items: Vec<ListItem> = if app.tree.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  (no inboxes)",
            Style::default().fg(rgb(th.c_dim())),
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
                        Style::default().fg(rgb(th.c_dim())),
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
        .highlight_style(sel_style(th, active));
    f.render_stateful_widget(list, area, &mut app.tree_state);
}

fn draw_items(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);
    app.list_height = panes[0].height.saturating_sub(2) as usize;
    draw_item_list(f, app, panes[0]);
    draw_preview(f, app, panes[1]);
}

fn draw_item_list(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let th = &app.theme;
    let active = matches!(app.pane, Pane::Items) && matches!(app.mode, Mode::List);
    let border = if active {
        rgb(th.c_accent())
    } else {
        rgb(th.c_border())
    };
    let title = {
        let p = app.selected_path();
        let name = if p.is_empty() {
            "items".into()
        } else {
            p.join("/")
        };
        if app.unread_only {
            format!(" {name}  unread ")
        } else {
            format!(" {name} ")
        }
    };
    let inner_w = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = if app.items.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "  empty",
            Style::default().fg(rgb(th.c_dim())),
        )))]
    } else {
        let inbox = app.chain().last().copied();
        let dim = Style::default().fg(rgb(th.c_dim()));
        app.items
            .iter()
            .map(|row| {
                let it = &row.item;
                let style = if it.read {
                    Style::default().fg(rgb(th.c_dim()))
                } else {
                    Style::default().fg(rgb(th.c_unread()))
                };
                let prefix = view_prefix(inbox, it);
                let src = source_label(&app.config, &it.source_id);
                ListItem::new(format_list_line(
                    it, row.count, src, &prefix, inner_w, style, dim,
                ))
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
        .highlight_style(sel_style(th, active));
    f.render_stateful_widget(list, area, &mut app.item_state);
}

fn format_list_line(
    item: &Item,
    count: usize,
    source: &str,
    prefix: &str,
    inner_w: usize,
    style: Style,
    dim: Style,
) -> Line<'static> {
    let mark = if item.read { " " } else { "*" };
    let when = short_time(&item.created_at);
    let src = pad_width(&trunc_width(source, 10), 10);
    let count_bit = if count > 1 {
        format!("·{count} ")
    } else {
        String::new()
    };
    let right = format!("{count_bit}{src}  {when}");
    let left = format!("{mark} ");
    let mid_w = inner_w.saturating_sub(display_width(&left) + display_width(&right));
    let (who, text) = row_who_text(item);
    let mut mid = format!("{prefix}{who}");
    if !text.is_empty() && text != who {
        mid.push_str("  ·  ");
        mid.push_str(&text);
    }
    let mid = pad_width(&trunc_width(&mid, mid_w), mid_w);
    Line::from(vec![
        Span::styled(left, style),
        Span::styled(mid, style.add_modifier(Modifier::BOLD)),
        Span::styled(right, dim),
    ])
}

fn draw_preview(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let border = rgb(th.c_border());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" preview ")
        .border_style(Style::default().fg(border));
    let Some(i) = app.item_state.selected() else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "nothing selected",
                Style::default().fg(rgb(th.c_dim())),
            ))
            .block(block),
            area,
        );
        return;
    };
    let Some(row) = app.items.get(i) else {
        return;
    };
    let it = &row.item;
    let from = it
        .from
        .as_ref()
        .map(|a| a.name.as_deref().filter(|s| !s.is_empty()).unwrap_or(&a.id))
        .filter(|s| !s.is_empty());
    let labels = if it.labels.is_empty() {
        "—".into()
    } else {
        it.labels.join(" ")
    };
    let mut text = it.title.clone();
    text.push('\n');
    if let Some(from) = from {
        text.push_str(&format!("from {from}\n"));
    }
    text.push_str(&format!("labels  {labels}\n\n"));
    text.push_str(&it.body);
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(block)
            .style(Style::default().fg(rgb(th.c_fg()))),
        area,
    );
}

fn draw_read(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let Some(i) = app.item_state.selected() else {
        f.render_widget(
            Paragraph::new("empty").block(Block::default().borders(Borders::ALL).title(" item ")),
            area,
        );
        return;
    };
    let Some(row) = app.items.get(i) else {
        return;
    };
    let it = &row.item;
    let labels = if it.labels.is_empty() {
        "—".into()
    } else {
        it.labels.join(" ")
    };
    let mut head = format!(
        "{}\n{}  {}  {}\nlabels  {labels}\n",
        it.title,
        it.source_id,
        short_time(&it.created_at),
        it.href.as_deref().unwrap_or(""),
    );
    if let Some(th) = it.thread.as_deref().filter(|s| !s.is_empty()) {
        let n = app
            .store
            .items_in_thread(th)
            .map(|v| v.len())
            .unwrap_or(1);
        head.push_str(&format!("thread {th}  {n}\n"));
    }
    if it.from.is_some()
        || !it.to.is_empty()
        || it.in_reply_to.is_some()
        || it.forward_of.is_some()
    {
        head.push_str(&cite_line(it));
        head.push('\n');
    }
    let mut text = format!("{head}\n{}", it.body);
    let show_parts = it.parts.len() > 1
        || it.parts.iter().any(|p| p.kind != paddock::PartKind::Text);
    if show_parts {
        text.push('\n');
        for p in &it.parts {
            text.push_str(&format!(
                "{}  {}  {}\n",
                p.kind.as_str(),
                p.mime,
                p.path.as_deref().unwrap_or(""),
            ));
        }
    }
    let p = Paragraph::new(text).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" item  esc back ")
            .border_style(Style::default().fg(rgb(th.c_accent()))),
    );
    f.render_widget(p, area);
}

fn draw_status(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let msg = match app.mode {
        Mode::Label => format!("label: {}_", app.label_buf),
        Mode::Search => format!("/{}_", app.search_buf),
        Mode::Command => format!(":{}_", app.cmd_buf),
        Mode::Compose => {
            if let Some(id) = app.compose_reply_to {
                format!("reply to #{id}")
            } else {
                "compose".into()
            }
        }
        _ => {
            if app.status.is_empty() {
                IDLE_HINT.into()
            } else {
                app.status.clone()
            }
        }
    };
    f.render_widget(
        Paragraph::new(msg).style(
            Style::default()
                .fg(rgb(th.c_dim()))
                .bg(rgb(th.c_bg())),
        ),
        area,
    );
}

fn draw_help(f: &mut ratatui::Frame, app: &App) {
    let th = &app.theme;
    let area = centered(f.area(), 52, 24);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(HELP).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" keys ")
                .border_style(Style::default().fg(rgb(th.c_accent()))),
        ),
        area,
    );
}

fn send_compose(app: &mut App) -> bool {
    let title = app.compose_title.clone();
    let body = app.compose_body.clone();
    let reply_to = app.compose_reply_to;
    app.mode = Mode::List;
    app.from_compose = false;
    app.exec(&Verb::Send {
        title,
        body,
        reply_to,
    })
}

fn handle_compose(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Esc => {
            app.mode = if app.from_read {
                Mode::Read
            } else {
                Mode::List
            };
            app.from_read = false;
            app.status = String::new();
        }
        KeyCode::Tab => {
            app.compose_field = match app.compose_field {
                ComposeField::Title => ComposeField::Body,
                ComposeField::Body => ComposeField::Title,
            };
        }
        KeyCode::Enter => match app.compose_field {
            ComposeField::Title => app.compose_field = ComposeField::Body,
            ComposeField::Body => app.compose_body.push('\n'),
        },
        KeyCode::Backspace => match app.compose_field {
            ComposeField::Title => {
                app.compose_title.pop();
            }
            ComposeField::Body => {
                app.compose_body.pop();
            }
        },
        KeyCode::Char('s') if ctrl => return send_compose(app),
        KeyCode::Char(':') if !ctrl => {
            app.from_compose = true;
            app.mode = Mode::Command;
            app.cmd_buf.clear();
        }
        KeyCode::Char(c) if !c.is_control() => match app.compose_field {
            ComposeField::Title => app.compose_title.push(c),
            ComposeField::Body => app.compose_body.push(c),
        },
        _ => {}
    }
    false
}

fn cite_line(it: &Item) -> String {
    let mut bits = Vec::new();
    if let Some(f) = &it.from {
        bits.push(format!("from {}", f.name.as_deref().unwrap_or(&f.id)));
    }
    if !it.to.is_empty() {
        let names: Vec<&str> = it
            .to
            .iter()
            .map(|a| a.name.as_deref().unwrap_or(&a.id))
            .collect();
        bits.push(format!("to {}", names.join(", ")));
    }
    if let Some(id) = it.in_reply_to {
        bits.push(format!("reply #{id}"));
    }
    if let Some(id) = it.forward_of {
        bits.push(format!("fwd #{id}"));
    }
    bits.join("  ")
}

fn draw_compose(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let th = &app.theme;
    let title_mark = if matches!(app.compose_field, ComposeField::Title) {
        ">"
    } else {
        " "
    };
    let body_mark = if matches!(app.compose_field, ComposeField::Body) {
        ">"
    } else {
        " "
    };
    let heading = if let Some(id) = app.compose_reply_to {
        format!(" reply to #{id} ")
    } else {
        " compose ".into()
    };
    let title = if matches!(app.compose_field, ComposeField::Title) {
        format!("{}{}_", app.compose_title, "")
    } else {
        app.compose_title.clone()
    };
    let body = if matches!(app.compose_field, ComposeField::Body) {
        format!("{}_", app.compose_body)
    } else {
        app.compose_body.clone()
    };
    let text = format!("{title_mark} title  {title}\n{body_mark} body\n{body}");
    let p = Paragraph::new(text).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(heading)
            .border_style(Style::default().fg(rgb(th.c_accent()))),
    );
    f.render_widget(p, area);
}

fn draw_label(f: &mut ratatui::Frame, app: &App) {
    let th = &app.theme;
    let area = centered(f.area(), 40, 3);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(format!(" {}", app.label_buf)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" label (enter toggle) ")
                .border_style(Style::default().fg(rgb(th.c_accent()))),
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
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| rfc.chars().take(16).collect())
}


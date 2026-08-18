use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::response::{Html, Redirect};
use axum::routing::get;
use axum::Router;
use paddock::{items_in_chain, load_or_init, pull_all, spawn_fs_watch, Config, Item, Paths, Store};
use serde::Deserialize;
use std::sync::Arc;

struct AppState {
    paths: Paths,
    store: Store,
}

pub fn serve(paths: Paths, bind: String) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(serve_async(paths, bind))
}

async fn serve_async(paths: Paths, bind: String) -> Result<()> {
    let (_cfg, store) = load_or_init(&paths)?;
    let _watch = spawn_fs_watch(store.clone(), paths.clone()).ok();
    let state = Arc::new(AppState { paths, store });
    let app = Router::new()
        .route("/", get(root))
        .route("/i/{*path}", get(inbox_page))
        .route("/item/{id}", get(item_page))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("paddock  http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn root(State(st): State<Arc<AppState>>) -> Redirect {
    let cfg = Config::load(&st.paths.config_file).unwrap_or_default();
    let name = cfg
        .inbox
        .first()
        .map(|i| i.name.as_str())
        .unwrap_or("all");
    Redirect::to(&format!("/i/{name}"))
}

#[derive(Deserialize)]
struct Q {
    pull: Option<String>,
    read: Option<i64>,
    unread: Option<i64>,
    toggle: Option<i64>,
    label: Option<String>,
    on: Option<i64>,
}

async fn inbox_page(
    State(st): State<Arc<AppState>>,
    Path(path): Path<String>,
    Query(q): Query<Q>,
) -> Result<Redirect, Html<String>> {
    let here = format!("/i/{path}");
    let cfg = Config::load(&st.paths.config_file).map_err(|e| html_err(&e.to_string()))?;
    if q.pull.is_some() {
        let _ = pull_all(&st.store, &cfg);
        return Ok(Redirect::to(&here));
    }
    if let Some(id) = q.toggle {
        let _ = st.store.toggle_read(id);
        return Ok(Redirect::to(&here));
    }
    if let Some(id) = q.read {
        let _ = st.store.set_read(id, true);
        return Ok(Redirect::to(&here));
    }
    if let Some(id) = q.unread {
        let _ = st.store.set_read(id, false);
        return Ok(Redirect::to(&here));
    }
    if let (Some(label), Some(id)) = (q.label.as_ref(), q.on) {
        let _ = st.store.toggle_label(id, label);
        return Ok(Redirect::to(&here));
    }
    Err(render_inbox(&st, &cfg, &path))
}

async fn item_page(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<Q>,
) -> Result<Redirect, Html<String>> {
    let here = format!("/item/{id}");
    if q.toggle.is_some() {
        let _ = st.store.toggle_read(id);
        return Ok(Redirect::to(&here));
    }
    if let Some(label) = q.label.as_ref() {
        let _ = st.store.toggle_label(id, label);
        return Ok(Redirect::to(&here));
    }
    match st.store.get(id) {
        Ok(item) => Err(render_item(&item)),
        Err(e) => Err(html_err(&e.to_string())),
    }
}

fn render_inbox(st: &AppState, cfg: &Config, path: &str) -> Html<String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let chain = cfg.find_chain(&parts);
    let items = match &chain {
        Some(c) => items_in_chain(&st.store, c).unwrap_or_default(),
        None => Vec::new(),
    };
    let nav = render_tree(cfg, &st.store, &parts);
    let list = if items.is_empty() {
        "<p class=\"empty\">empty</p>".into()
    } else {
        let mut rows = String::from("<table><thead><tr><th></th><th>title</th><th>source</th><th>when</th><th></th></tr></thead><tbody>");
        for it in &items {
            let mark = if it.read { "" } else { "*" };
            let cls = if it.read { "read" } else { "unread" };
            let when = short_time(&it.created_at);
            rows.push_str(&format!(
                "<tr class=\"{cls}\"><td class=\"mark\">{mark}</td><td><a href=\"/item/{id}\">{title}</a></td><td class=\"dim\">{src}</td><td class=\"dim\">{when}</td><td><a href=\"/i/{path}?toggle={id}\">read</a></td></tr>",
                id = it.id,
                title = esc(&it.title),
                src = esc(&it.source_id),
                path = esc(path),
            ));
        }
        rows.push_str("</tbody></table>");
        rows
    };
    let crumb = parts.join(" / ");
    let missing = if chain.is_none() {
        format!("<p class=\"empty\">no inbox named {}</p>", esc(path))
    } else {
        String::new()
    };
    page(
        &crumb,
        &format!(
            r#"<div class="wrap">
<nav>{nav}</nav>
<main>
<header class="bar"><h1>{crumb}</h1><a href="/i/{path}?pull=1">pull</a></header>
{missing}
{list}
</main>
</div>"#,
            crumb = esc(&crumb),
            path = esc(path),
        ),
    )
}

fn render_tree(cfg: &Config, store: &Store, selected: &[&str]) -> String {
    let mut out = String::from("<ul class=\"tree\">");
    walk_nav(cfg, &cfg.inbox, &[], selected, store, &mut out);
    out.push_str("</ul>");
    out
}

fn walk_nav(
    cfg: &Config,
    inboxes: &[paddock::InboxConfig],
    prefix: &[String],
    selected: &[&str],
    store: &Store,
    out: &mut String,
) {
    for ib in inboxes {
        let mut path = prefix.to_vec();
        path.push(ib.name.clone());
        let refs: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
        let (unread, total) = match cfg.find_chain(&refs) {
            Some(chain) => {
                let items = items_in_chain(store, &chain).unwrap_or_default();
                (items.iter().filter(|i| !i.read).count(), items.len())
            }
            None => (0, 0),
        };
        let href = format!("/i/{}", path.join("/"));
        let sel = refs == selected;
        let cls = if sel { "sel" } else { "" };
        out.push_str(&format!(
            "<li class=\"d{depth} {cls}\"><a href=\"{href}\">{name} <span class=\"dim\">{unread}/{total}</span></a></li>",
            depth = prefix.len(),
            name = esc(&ib.name),
        ));
        walk_nav(cfg, &ib.inbox, &path, selected, store, out);
    }
}

fn render_item(it: &Item) -> Html<String> {
    let labels = if it.labels.is_empty() {
        "<span class=\"dim\">—</span>".into()
    } else {
        it.labels
            .iter()
            .map(|l| {
                format!(
                    "<a class=\"lab\" href=\"/item/{id}?label={l}\">{l}</a>",
                    id = it.id,
                    l = esc(l)
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mark = if it.read { "unread" } else { "read" };
    page(
        &it.title,
        &format!(
            r#"<main class="item">
<p class="bar"><a href="/">← inboxes</a> · <a href="/item/{id}?toggle=1">{mark}</a></p>
<h1>{title}</h1>
<p class="meta">{src} · {when} · {href}</p>
<p class="labels">labels {labels} · <a href="/item/{id}?label=later">later</a></p>
<pre>{body}</pre>
</main>"#,
            id = it.id,
            title = esc(&it.title),
            src = esc(&it.source_id),
            when = esc(&short_time(&it.created_at)),
            href = esc(it.href.as_deref().unwrap_or("")),
            body = esc(&it.body),
        ),
    )
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="15">
<title>{title} · paddock</title>
<style>{css}</style>
</head>
<body>
{body}
</body>
</html>"#,
        title = esc(title),
        css = CSS,
    ))
}

fn html_err(msg: &str) -> Html<String> {
    page("error", &format!("<main><pre>{}</pre></main>", esc(msg)))
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn short_time(rfc: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| rfc.chars().take(16).collect())
}

const CSS: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: #0a0a0a; color: #d2d2c8; }
body { font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
a { color: #d8c070; text-decoration: none; }
a:hover { text-decoration: underline; }
.wrap { display: flex; min-height: 100vh; }
nav { width: 220px; border-right: 1px solid #222; padding: 12px 0; flex-shrink: 0; }
nav .tree { list-style: none; margin: 0; padding: 0; }
nav li { padding: 2px 14px; }
nav li.d1 { padding-left: 28px; }
nav li.d2 { padding-left: 42px; }
nav li.d3 { padding-left: 56px; }
nav li.sel a { color: #fff; font-weight: 700; }
main { flex: 1; padding: 12px 18px 32px; min-width: 0; }
.bar { display: flex; gap: 16px; align-items: baseline; margin-bottom: 10px; }
.bar h1 { font-size: 13px; font-weight: 700; margin: 0; }
h1 { font-size: 16px; margin: 0 0 8px; }
table { width: 100%; border-collapse: collapse; }
th { text-align: left; color: #666; font-weight: 400; border-bottom: 1px solid #222; padding: 4px 8px; }
td { padding: 3px 8px; border-bottom: 1px solid #161616; vertical-align: top; }
td.mark { width: 1.2em; color: #d8c070; }
.dim { color: #6a6a66; }
tr.read td, tr.read a { color: #666; }
.empty { color: #555; }
.meta, .labels { color: #8a8a84; }
pre { white-space: pre-wrap; word-break: break-word; margin: 16px 0 0; }
.lab { border: 1px solid #333; padding: 0 4px; }
"#;

use anyhow::Result;
use axum::extract::{Path, Query, State};
use axum::response::{Html, Redirect};
use axum::routing::get;
use axum::Router;
use paddock::cmd::{run_verb, VerbCtx};
use paddock::keys::{bindings_json, Verb};
use paddock::theme::{load_theme, Theme};
use paddock::{
    items_in_chain, load_or_init, pull_all, relabel, spawn_fs_watch, Config, Item, Paths, Store,
};
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
        .route("/x/{verb}", get(exec_x))
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
    msg: Option<String>,
    only: Option<String>,
}

#[derive(Deserialize)]
struct Xq {
    item: Option<i64>,
    inbox: Option<String>,
    arg: Option<String>,
    only: Option<String>,
}

async fn exec_x(
    State(st): State<Arc<AppState>>,
    Path(verb): Path<String>,
    Query(q): Query<Xq>,
) -> Result<Redirect, Html<String>> {
    let cfg = Config::load(&st.paths.config_file).map_err(|e| html_err(&st, &e.to_string()))?;
    let parsed = Verb::from_id(&verb, q.arg.as_deref());
    let inbox = q.inbox.clone().unwrap_or_else(|| {
        cfg.inbox
            .first()
            .map(|i| i.name.clone())
            .unwrap_or_else(|| "all".into())
    });
    let dest_base = if q.inbox.is_some() {
        format!("/i/{inbox}")
    } else if let Some(id) = q.item {
        format!("/item/{id}")
    } else {
        format!("/i/{inbox}")
    };
    let Some(verb) = parsed else {
        return Ok(Redirect::to(&with_qs(
            &dest_base,
            &format!("not an editor command: {verb}"),
            q.only.as_deref(),
        )));
    };
    let ctx = VerbCtx {
        item_id: q.item.filter(|&i| i > 0),
        inbox_path: inbox
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect(),
        unread_only: q.only.as_deref() == Some("1"),
    };
    match run_verb(&st.store, &cfg, &st.paths, &ctx, &verb) {
        Ok(out) => {
            let only = out
                .unread_only
                .map(|u| if u { "1" } else { "0" })
                .or(q.only.as_deref().map(|s| if s == "1" { "1" } else { "0" }));
            let msg = if !out.status.is_empty() {
                out.status
            } else {
                out.overlay.unwrap_or_default()
            };
            Ok(Redirect::to(&with_qs(&dest_base, &msg, only)))
        }
        Err(e) => Ok(Redirect::to(&with_qs(&dest_base, &e.to_string(), q.only.as_deref()))),
    }
}

fn with_qs(base: &str, msg: &str, only: Option<&str>) -> String {
    let mut parts = Vec::new();
    if only == Some("1") {
        parts.push("only=1".into());
    }
    if !msg.is_empty() {
        parts.push(format!("msg={}", urlenc(msg)));
    }
    if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", parts.join("&"))
    }
}

async fn inbox_page(
    State(st): State<Arc<AppState>>,
    Path(path): Path<String>,
    Query(q): Query<Q>,
) -> Result<Redirect, Html<String>> {
    let here = format!("/i/{path}");
    let cfg = Config::load(&st.paths.config_file).map_err(|e| html_err(&st, &e.to_string()))?;
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
        let _ = relabel(&st.store, &cfg, id, label);
        return Ok(Redirect::to(&here));
    }
    Err(render_inbox(&st, &cfg, &path, q.msg.as_deref(), q.only.as_deref() == Some("1")))
}

async fn item_page(
    State(st): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(q): Query<Q>,
) -> Result<Redirect, Html<String>> {
    let here = format!("/item/{id}");
    let cfg = Config::load(&st.paths.config_file).map_err(|e| html_err(&st, &e.to_string()))?;
    if q.toggle.is_some() {
        let _ = st.store.toggle_read(id);
        return Ok(Redirect::to(&here));
    }
    if let Some(label) = q.label.as_ref() {
        let _ = relabel(&st.store, &cfg, id, label);
        return Ok(Redirect::to(&here));
    }
    match st.store.get(id) {
        Ok(item) => Err(render_item(&st, &item, q.msg.as_deref())),
        Err(e) => Err(html_err(&st, &e.to_string())),
    }
}

fn theme_of(st: &AppState) -> Theme {
    let cfg = Config::load(&st.paths.config_file).unwrap_or_default();
    load_theme(&cfg, &st.paths)
}

fn render_inbox(st: &AppState, cfg: &Config, path: &str, msg: Option<&str>, only: bool) -> Html<String> {
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let chain = cfg.find_chain(&parts);
    let mut items = match &chain {
        Some(c) => items_in_chain(&st.store, c).unwrap_or_default(),
        None => Vec::new(),
    };
    if only {
        items.retain(|i| !i.read);
    }
    let nav = render_tree(cfg, &st.store, &parts);
    let list = if items.is_empty() {
        "<p class=\"empty\">empty</p>".into()
    } else {
        let mut rows = String::from("<table><thead><tr><th></th><th>title</th><th>source</th><th>when</th><th></th></tr></thead><tbody>");
        for (n, it) in items.iter().enumerate() {
            let mark = if it.read { "" } else { "*" };
            let cls = if it.read { "read" } else { "unread" };
            let cur = if n == 0 { " cur" } else { "" };
            let when = short_time(&it.created_at);
            let prefix = chain
                .as_ref()
                .and_then(|c| c.last())
                .map(|ib| match ib.view_kind() {
                    "board" => format!("[{}] ", ib.board_column(it).unwrap_or("—")),
                    "calendar" => it
                        .start
                        .as_deref()
                        .map(|s| format!("{} ", s.chars().take(10).collect::<String>()))
                        .unwrap_or_default(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            rows.push_str(&format!(
                "<tr class=\"{cls}{cur}\" data-id=\"{id}\"><td class=\"mark\">{mark}</td><td><a href=\"/item/{id}\">{title}</a></td><td class=\"dim\">{src}</td><td class=\"dim\">{when}</td><td><a href=\"/i/{path}?toggle={id}\">read</a></td></tr>",
                id = it.id,
                title = esc(&format!("{prefix}{}", it.title)),
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
    let status = msg.unwrap_or("");
    page(
        &theme_of(st),
        &crumb,
        &format!(
            r#"<div class="wrap">
<nav>{nav}</nav>
<main>
<header class="bar"><h1>{crumb}</h1><a href="/i/{path}?pull=1">pull</a></header>
{missing}
{list}
</main>
</div>
<p class="status">{status}</p>"#,
            crumb = esc(&crumb),
            path = esc(path),
            status = esc(status),
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

fn render_item(st: &AppState, it: &Item, msg: Option<&str>) -> Html<String> {
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
    let status = msg.unwrap_or("");
    let thread_line = match it.thread.as_deref().filter(|s| !s.is_empty()) {
        Some(th) => {
            let n = st
                .store
                .items_in_thread(th)
                .map(|v| v.len())
                .unwrap_or(1);
            format!(
                "<p class=\"meta\">thread {th} · {n}</p>",
                th = esc(th),
            )
        }
        None => String::new(),
    };
    let show_parts = it.parts.len() > 1
        || it.parts.iter().any(|p| p.kind != paddock::PartKind::Text);
    let parts_html = if show_parts {
        let mut s = String::from("<ul class=\"parts\">");
        for p in &it.parts {
            s.push_str(&format!(
                "<li>{} {} {}</li>",
                esc(p.kind.as_str()),
                esc(&p.mime),
                esc(p.path.as_deref().unwrap_or("")),
            ));
        }
        s.push_str("</ul>");
        s
    } else {
        String::new()
    };
    page(
        &theme_of(st),
        &it.title,
        &format!(
            r#"<main class="item">
<p class="bar"><a href="/">← inboxes</a> · <a href="/item/{id}?toggle=1">{mark}</a></p>
<h1>{title}</h1>
<p class="meta">{src} · {when} · {href}</p>
{thread_line}
<p class="labels">labels {labels} · <a href="/item/{id}?label=later">later</a> · <a href="/item/{id}?label=todo">todo</a></p>
<pre>{body}</pre>
{parts_html}
</main>
<p class="status">{status}</p>"#,
            id = it.id,
            title = esc(&it.title),
            src = esc(&it.source_id),
            when = esc(&short_time(&it.created_at)),
            href = esc(it.href.as_deref().unwrap_or("")),
            body = esc(&it.body),
            status = esc(status),
        ),
    )
}

fn page(theme: &Theme, title: &str, body: &str) -> Html<String> {
    let css = CSS.replace("/*VARS*/", &theme.css_vars());
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
<div id="cmdwrap" hidden><span>:</span><input id="cmd" autocomplete="off" spellcheck="false"></div>
<div id="helpov" hidden><pre id="helptext"></pre></div>
<script type="application/json" id="bindings">{bindings}</script>
<script>{js}</script>
</body>
</html>"#,
        title = esc(title),
        css = css,
        bindings = bindings_json(),
        js = JS,
    ))
}

fn html_err(st: &AppState, msg: &str) -> Html<String> {
    page(
        &theme_of(st),
        "error",
        &format!("<main><pre>{}</pre></main>", esc(msg)),
    )
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

fn urlenc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b' ' => o.push('+'),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn short_time(rfc: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc)
        .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| rfc.chars().take(16).collect())
}

const CSS: &str = r#"
:root { /*VARS*/ color-scheme: dark; }
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: var(--bg); color: var(--fg); }
body { font: 13px/1.45 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.wrap { display: flex; min-height: 100vh; }
nav { width: 220px; border-right: 1px solid var(--border); padding: 12px 0; flex-shrink: 0; }
nav .tree { list-style: none; margin: 0; padding: 0; }
nav li { padding: 2px 14px; }
nav li.d1 { padding-left: 28px; }
nav li.d2 { padding-left: 42px; }
nav li.d3 { padding-left: 56px; }
nav li.sel a { color: var(--unread); font-weight: 700; }
main { flex: 1; padding: 12px 18px 32px; min-width: 0; }
.bar { display: flex; gap: 16px; align-items: baseline; margin-bottom: 10px; }
.bar h1 { font-size: 13px; font-weight: 700; margin: 0; }
h1 { font-size: 16px; margin: 0 0 8px; }
table { width: 100%; border-collapse: collapse; }
th { text-align: left; color: var(--dim); font-weight: 400; border-bottom: 1px solid var(--border); padding: 4px 8px; }
td { padding: 3px 8px; border-bottom: 1px solid var(--border); vertical-align: top; }
td.mark { width: 1.2em; color: var(--accent); }
.dim { color: var(--dim); }
tr.read td, tr.read a { color: var(--dim); }
tr.unread td { color: var(--unread); }
tr.cur { background: var(--select); }
.empty { color: var(--dim); }
.meta, .labels { color: var(--dim); }
pre { white-space: pre-wrap; word-break: break-word; margin: 16px 0 0; }
.lab { border: 1px solid var(--border); padding: 0 4px; }
.parts { list-style: none; margin: 12px 0 0; padding: 0; color: var(--dim); }
.parts li { padding: 2px 0; }
.status { position: fixed; bottom: 0; left: 0; right: 0; padding: 2px 8px; color: var(--dim); background: var(--bg); }
#cmdwrap { position: fixed; bottom: 0; left: 0; right: 0; background: var(--bg); border-top: 1px solid var(--border); padding: 4px 8px; z-index: 2; }
#cmdwrap input { background: transparent; border: 0; color: var(--fg); font: inherit; width: 90%; outline: none; }
#helpov { position: fixed; inset: 10% 18%; background: var(--bg); border: 1px solid var(--accent); padding: 12px; overflow: auto; z-index: 3; }
"#;

const JS: &str = r#"
(function(){
  var el = document.getElementById('bindings');
  if(!el) return;
  var B = JSON.parse(el.textContent);
  var pending = '';
  var lastSearch = '';
  var cmdwrap = document.getElementById('cmdwrap');
  var cmd = document.getElementById('cmd');
  var helpov = document.getElementById('helpov');
  var helptext = document.getElementById('helptext');
  if(helptext) helptext.textContent = B.help || '';

  function rows(){ return Array.prototype.slice.call(document.querySelectorAll('tr[data-id]')); }
  function cur(){ return document.querySelector('tr[data-id].cur'); }
  function select(row){
    rows().forEach(function(r){ r.classList.remove('cur'); });
    if(row){ row.classList.add('cur'); row.scrollIntoView({block:'nearest'}); }
  }
  function itemId(){
    var r = cur();
    if(r) return r.getAttribute('data-id');
    var m = location.pathname.match(/^\/item\/(\d+)/);
    return m ? m[1] : '';
  }
  function inbox(){
    var m = location.pathname.match(/^\/i\/(.+)/);
    return m ? m[1] : '';
  }
  function only(){
    return new URLSearchParams(location.search).get('only') === '1' ? '1' : '';
  }
  function keyName(e){
    if(e.ctrlKey && e.key && e.key.length===1) return 'C-'+e.key.toLowerCase();
    if(e.key===' ') return ' ';
    if(e.key==='Enter') return 'Enter';
    if(e.key==='Escape') return 'Esc';
    if(e.key==='Tab') return 'Tab';
    if(e.key==='ArrowDown') return 'Down';
    if(e.key==='ArrowUp') return 'Up';
    if(e.key==='ArrowLeft') return 'Left';
    if(e.key==='ArrowRight') return 'Right';
    if(e.key && e.key.length===1) return e.key;
    return '';
  }
  function findKey(seq){
    for(var i=0;i<B.keys.length;i++) if(B.keys[i].seq===seq) return B.keys[i];
    return null;
  }
  function isPrefix(seq){
    for(var i=0;i<B.keys.length;i++){
      var s = B.keys[i].seq;
      if(s.length>seq.length && s.indexOf(seq)===0) return true;
    }
    return false;
  }
  function runLocal(verb){
    var rs = rows();
    var i = rs.findIndex(function(r){ return r.classList.contains('cur'); });
    if(i<0) i = 0;
    if(verb==='down' && rs[i+1]) select(rs[i+1]);
    else if(verb==='up' && rs[Math.max(0,i-1)]) select(rs[Math.max(0,i-1)]);
    else if(verb==='top' && rs[0]) select(rs[0]);
    else if(verb==='bottom' && rs.length) select(rs[rs.length-1]);
    else if(verb==='half-page-down' && rs.length) select(rs[Math.min(rs.length-1, i+8)]);
    else if(verb==='half-page-up' && rs.length) select(rs[Math.max(0, i-8)]);
    else if(verb==='page-down' && rs.length) select(rs[Math.min(rs.length-1, i+20)]);
    else if(verb==='page-up' && rs.length) select(rs[Math.max(0, i-20)]);
    else if(verb==='open-read'){ var r=cur(); if(r) location.href='/item/'+r.getAttribute('data-id'); }
    else if(verb==='pane-tree' || verb==='escape'){
      if(helpov && !helpov.hidden){ helpov.hidden = true; return; }
      if(location.pathname.indexOf('/item/')===0) location.href='/';
    }
    else if(verb==='next-inbox' || verb==='prev-inbox'){
      var links = Array.prototype.slice.call(document.querySelectorAll('nav a'));
      var here = location.pathname;
      var idx = links.findIndex(function(a){ return a.getAttribute('href')===here; });
      if(idx<0) idx = 0;
      idx += verb==='next-inbox' ? 1 : -1;
      if(links[idx]) location.href = links[idx].getAttribute('href');
    }
    else if(verb==='help'){ if(helpov) helpov.hidden = !helpov.hidden; }
    else if(verb==='command'){ if(cmdwrap){ cmdwrap.hidden=false; cmd.value=''; cmd.focus(); } }
    else if(verb==='search' || verb==='search-next' || verb==='search-prev'){
      var q = lastSearch;
      if(verb==='search'){ q = prompt('search') || ''; lastSearch = q; }
      if(!q) return;
      var needle = q.toLowerCase();
      var hits = rs.filter(function(r){ return (r.textContent||'').toLowerCase().indexOf(needle)>=0; });
      if(!hits.length) return;
      var curI = hits.indexOf(cur());
      var next = hits[0];
      if(verb==='search-next' && curI>=0) next = hits[(curI+1)%hits.length];
      if(verb==='search-prev' && curI>=0) next = hits[(curI-1+hits.length)%hits.length];
      select(next);
    }
    else if(verb==='label-prompt'){
      var name = prompt('label');
      if(name) runRemote('relabel', name);
    }
  }
  function runRemote(verb, arg){
    var u = '/x/'+encodeURIComponent(verb)
      +'?item='+encodeURIComponent(itemId()||'0')
      +'&inbox='+encodeURIComponent(inbox())
      +'&arg='+encodeURIComponent(arg||'')
      +'&only='+encodeURIComponent(only());
    location.href = u;
  }
  function dispatch(binding, arg){
    if(binding.local) runLocal(binding.verb);
    else runRemote(binding.verb, arg);
  }
  document.addEventListener('keydown', function(e){
    if(cmd && document.activeElement===cmd) return;
    if(e.target && (e.target.tagName==='INPUT' || e.target.tagName==='TEXTAREA')) return;
    var part = keyName(e);
    if(!part) return;
    if(part==='Tab') e.preventDefault();
    var seq = pending + part;
    if(isPrefix(seq)){ pending = seq; e.preventDefault(); return; }
    var hit = findKey(seq);
    pending = '';
    if(hit){ e.preventDefault(); dispatch(hit, ''); return; }
    if(seq!==part){
      if(isPrefix(part)){ pending = part; e.preventDefault(); return; }
      var hit2 = findKey(part);
      if(hit2){ e.preventDefault(); dispatch(hit2, ''); }
    }
  });
  if(cmd){
    cmd.addEventListener('keydown', function(e){
      if(e.key==='Escape'){ cmdwrap.hidden=true; cmd.blur(); e.preventDefault(); }
      if(e.key==='Enter'){
        e.preventDefault();
        var raw = cmd.value.replace(/^\s+|\s+$/g,'');
        cmdwrap.hidden=true; cmd.blur();
        var word = (raw.split(/\s+/)[0]||'');
        var rest = raw.slice(word.length).replace(/^\s+/,'');
        var c = null;
        for(var i=0;i<B.commands.length;i++) if(B.commands[i].name===word) c = B.commands[i];
        if(!c){
          var st = document.querySelector('.status');
          if(st) st.textContent = (B.unknown_prefix||'not an editor command: ')+word;
          return;
        }
        if(c.verb==='help' || c.verb==='quit' || c.verb==='command'){ runLocal(c.verb); return; }
        runRemote(c.verb, rest);
      }
    });
  }
  if(rows().length && !cur()) select(rows()[0]);
})();
"#;

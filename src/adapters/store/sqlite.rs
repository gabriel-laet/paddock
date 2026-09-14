//! SQLite behind the `Store` port. Everything lives in the one file, part
//! bytes included, so a key encrypts the lot (SQLCipher).

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::kernel::{
    Actor, ActorKind, By, Fact, Item, Label, NewItem, NewPart, Part, PartKind, Question, StaleHint,
    Store,
};

const ITEM_COLS: &str =
    "id, source_id, foreign_id, title, body, href, start, end, created_at, read, thread,      from_id, from_name, from_kind, in_reply_to, forward_of, cite_excerpt,      cite_actor_id, cite_actor_name, cite_actor_kind";

#[derive(Clone)]
pub struct Sqlite {
    conn: Arc<Mutex<Connection>>,
}

impl Sqlite {
    /// Open (or create) the store. With a key the file is encrypted; an
    /// existing plaintext store opened with a key fails as "not a database".
    pub fn open(path: &Path, key: Option<&str>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        if let Some(key) = key {
            conn.pragma_update(None, "key", key)?;
        }
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .with_context(|| match key {
                Some(_) => format!(
                    "open {}: wrong key, or not an encrypted store",
                    path.display()
                ),
                None => format!("open {}: not a store, or it is encrypted", path.display()),
            })?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY,
                source_id TEXT NOT NULL,
                foreign_id TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                href TEXT,
                start TEXT,
                end TEXT,
                thread TEXT,
                created_at TEXT NOT NULL,
                read INTEGER NOT NULL DEFAULT 0,
                UNIQUE(source_id, foreign_id)
            );
            CREATE TABLE IF NOT EXISTS labels (
                item_id INTEGER NOT NULL,
                label TEXT NOT NULL,
                PRIMARY KEY (item_id, label),
                FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS parts (
                id INTEGER PRIMARY KEY,
                item_id INTEGER NOT NULL,
                seq INTEGER NOT NULL,
                kind TEXT NOT NULL,
                mime TEXT NOT NULL,
                text TEXT,
                blob BLOB,
                FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS item_to (
                item_id INTEGER NOT NULL,
                actor_id TEXT NOT NULL,
                name TEXT,
                kind TEXT NOT NULL,
                PRIMARY KEY (item_id, actor_id),
                FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS llm_classified (
                item_id INTEGER NOT NULL,
                classifier_id TEXT NOT NULL,
                PRIMARY KEY (item_id, classifier_id),
                FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_parts_item ON parts(item_id);
            CREATE INDEX IF NOT EXISTS idx_item_to_item ON item_to(item_id);
            "#,
        )?;
        ensure_column(&conn, "items", "start", "TEXT")?;
        ensure_column(&conn, "items", "end", "TEXT")?;
        ensure_column(&conn, "items", "thread", "TEXT")?;
        ensure_column(&conn, "items", "from_id", "TEXT")?;
        ensure_column(&conn, "items", "from_name", "TEXT")?;
        ensure_column(&conn, "items", "from_kind", "TEXT")?;
        ensure_column(&conn, "items", "in_reply_to", "INTEGER")?;
        ensure_column(&conn, "items", "forward_of", "INTEGER")?;
        ensure_column(&conn, "items", "cite_excerpt", "TEXT")?;
        ensure_column(&conn, "items", "cite_actor_id", "TEXT")?;
        ensure_column(&conn, "items", "cite_actor_name", "TEXT")?;
        ensure_column(&conn, "items", "cite_actor_kind", "TEXT")?;
        ensure_column(&conn, "items", "in_reply_to_foreign", "TEXT")?;
        ensure_column(&conn, "items", "forward_of_foreign", "TEXT")?;
        ensure_column(&conn, "parts", "blob", "BLOB")?;
        ensure_column(&conn, "labels", "by", "TEXT NOT NULL DEFAULT 'hand'")?;
        ensure_column(&conn, "labels", "at", "TEXT NOT NULL DEFAULT ''")?;
        ensure_column(&conn, "labels", "removed", "INTEGER NOT NULL DEFAULT 0")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_items_thread ON items(thread);
             CREATE INDEX IF NOT EXISTS idx_items_reply_foreign ON items(source_id, in_reply_to_foreign);
             CREATE INDEX IF NOT EXISTS idx_items_fwd_foreign ON items(source_id, forward_of_foreign);
             CREATE INDEX IF NOT EXISTS idx_items_start ON items(start);
             CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at);",
        )?;
        backfill_parts(&conn)?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(item_id UNINDEXED, title, text);
             CREATE TABLE IF NOT EXISTS vectors (
                item_id INTEGER PRIMARY KEY,
                data BLOB NOT NULL,
                FOREIGN KEY (item_id) REFERENCES items(id) ON DELETE CASCADE
             );",
        )?;
        let indexed: i64 = conn.query_row("SELECT COUNT(*) FROM items_fts", [], |r| r.get(0))?;
        if indexed == 0 {
            conn.execute_batch("DELETE FROM items_fts;")?;
            conn.execute(
                &format!("INSERT INTO items_fts(item_id, title, text) {FTS_ROWS}"),
                [],
            )?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("store lock: {e}"))
    }
}

impl Store for Sqlite {
    fn upsert(&self, item: &NewItem) -> Result<(i64, bool)> {
        let body = preview_body(item);
        let thread = trim_thread(item.thread.as_deref());
        let to_write = parts_to_insert(item);
        let (from_id, from_name, from_kind) = actor_cols(item.from.as_ref());
        let (cite_id, cite_name, cite_kind) = actor_cols(item.cite_actor.as_ref());
        let reply_f = trim_opt(item.in_reply_to.as_deref());
        let fwd_f = trim_opt(item.forward_of.as_deref());
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let reply_id = match reply_f.as_deref() {
            Some(f) => lookup_id(&tx, &item.source_id, f)?,
            None => None,
        };
        let fwd_id = match fwd_f.as_deref() {
            Some(f) => lookup_id(&tx, &item.source_id, f)?,
            None => None,
        };
        let existing = lookup_id(&tx, &item.source_id, &item.foreign_id)?;
        let (id, created) = if let Some(id) = existing {
            tx.execute(
                "UPDATE items SET title = ?1, body = ?2, href = ?3 WHERE id = ?4",
                params![item.title, body, item.href, id],
            )?;
            if item.read == Some(true) {
                tx.execute("UPDATE items SET read = 1 WHERE id = ?1", params![id])?;
            }
            if thread.is_some() {
                tx.execute(
                    "UPDATE items SET thread = ?1 WHERE id = ?2",
                    params![thread, id],
                )?;
            }
            if item.start.is_some() {
                tx.execute(
                    "UPDATE items SET start = ?1 WHERE id = ?2",
                    params![item.start, id],
                )?;
            }
            if item.end.is_some() {
                tx.execute(
                    "UPDATE items SET end = ?1 WHERE id = ?2",
                    params![item.end, id],
                )?;
            }
            if item.from.is_some() {
                tx.execute(
                    "UPDATE items SET from_id = ?1, from_name = ?2, from_kind = ?3 WHERE id = ?4",
                    params![from_id, from_name, from_kind, id],
                )?;
            }
            if reply_f.is_some() {
                tx.execute(
                    "UPDATE items SET in_reply_to_foreign = ?1, in_reply_to = ?2 WHERE id = ?3",
                    params![reply_f, reply_id, id],
                )?;
            }
            if fwd_f.is_some() {
                tx.execute(
                    "UPDATE items SET forward_of_foreign = ?1, forward_of = ?2 WHERE id = ?3",
                    params![fwd_f, fwd_id, id],
                )?;
            }
            if trim_opt(item.cite_excerpt.as_deref()).is_some() {
                tx.execute(
                    "UPDATE items SET cite_excerpt = ?1 WHERE id = ?2",
                    params![trim_opt(item.cite_excerpt.as_deref()), id],
                )?;
            }
            if item.cite_actor.is_some() {
                tx.execute(
                    "UPDATE items SET cite_actor_id = ?1, cite_actor_name = ?2, cite_actor_kind = ?3 WHERE id = ?4",
                    params![cite_id, cite_name, cite_kind, id],
                )?;
            }
            if !to_write.is_empty() {
                tx.execute("DELETE FROM parts WHERE item_id = ?1", params![id])?;
                for (seq, part) in to_write.iter().enumerate() {
                    insert_part_row(&tx, id, seq as i64, part)?;
                }
            }
            if !item.to.is_empty() {
                tx.execute("DELETE FROM item_to WHERE item_id = ?1", params![id])?;
                insert_to_rows(&tx, id, &item.to)?;
            }
            (id, false)
        } else {
            tx.execute(
                "INSERT INTO items
                    (source_id, foreign_id, title, body, href, start, end, thread, created_at, read,
                     from_id, from_name, from_kind, in_reply_to, forward_of, cite_excerpt,
                     cite_actor_id, cite_actor_name, cite_actor_kind,
                     in_reply_to_foreign, forward_of_foreign)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    item.source_id,
                    item.foreign_id,
                    item.title,
                    body,
                    item.href,
                    item.start,
                    item.end,
                    thread,
                    created_at,
                    item.read.unwrap_or(false),
                    from_id,
                    from_name,
                    from_kind,
                    reply_id,
                    fwd_id,
                    trim_opt(item.cite_excerpt.as_deref()),
                    cite_id,
                    cite_name,
                    cite_kind,
                    reply_f,
                    fwd_f
                ],
            )?;
            let id = tx.last_insert_rowid();
            for (seq, part) in to_write.iter().enumerate() {
                insert_part_row(&tx, id, seq as i64, part)?;
            }
            insert_to_rows(&tx, id, &item.to)?;
            (id, true)
        };
        stitch_cites(&tx, &item.source_id, &item.foreign_id, id)?;
        index_text(&tx, id)?;
        tx.commit()?;
        Ok((id, created))
    }

    fn get(&self, id: i64) -> Result<Item> {
        let conn = self.lock()?;
        let mut item = conn.query_row(
            &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
            params![id],
            row_item,
        )?;
        (item.labels, item.denied) = labels_for(&conn, id)?;
        item.parts = parts_for(&conn, id)?;
        item.to = to_for(&conn, id)?;
        ensure_text_part(&conn, &mut item)?;
        Ok(item)
    }

    fn stale(&self) -> Result<Vec<StaleHint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id, source_id, created_at, start, end FROM items")?;
        let mut hints: Vec<StaleHint> = stmt
            .query_map([], |row| {
                Ok(StaleHint {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    created_at: row.get(2)?,
                    start: row.get(3)?,
                    end: row.get(4)?,
                    labels: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut lab_stmt = conn.prepare("SELECT item_id, label FROM labels WHERE removed = 0")?;
        let labs = lab_stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
        for row in labs {
            let (id, label) = row?;
            map.entry(id).or_default().push(label);
        }
        for h in &mut hints {
            h.labels = map.remove(&h.id).unwrap_or_default();
        }
        Ok(hints)
    }

    fn ask(&self, filter: &Question) -> Result<Vec<Item>> {
        let (where_sql, params) = filter_where(filter);
        let order = filter_order(filter);
        let conn = self.lock()?;
        let sql = format!("SELECT {ITEM_COLS} FROM items {where_sql} {order}");
        let mut stmt = conn.prepare(&sql)?;
        let mut items: Vec<Item> = stmt
            .query_map(params_from_iter(params), row_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if let Some(near) = &filter.near {
            items = rank(&conn, items, near)?;
            items.truncate(filter.limit.unwrap_or(20));
        } else if let Some(limit) = filter.limit {
            items.truncate(limit);
        }
        hydrate(&conn, &mut items)?;
        Ok(items)
    }

    fn count(&self, filter: &Question) -> Result<usize> {
        let (where_sql, params) = filter_where(filter);
        let conn = self.lock()?;
        let sql = format!("SELECT COUNT(*) FROM items {where_sql}");
        let n: i64 = conn.query_row(&sql, params_from_iter(params), |row| row.get(0))?;
        Ok(n as usize)
    }

    fn unembedded(&self) -> Result<Vec<i64>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id FROM items WHERE id NOT IN (SELECT item_id FROM vectors) ORDER BY id",
        )?;
        let ids = stmt.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<rusqlite::Result<Vec<i64>>>()?)
    }

    fn counts_by_source(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT source_id, COUNT(*) FROM items GROUP BY source_id ORDER BY source_id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn note(&self, id: i64, fact: Fact) -> Result<()> {
        let conn = self.lock()?;
        match fact {
            Fact::Read(read) => {
                conn.execute(
                    "UPDATE items SET read = ?1 WHERE id = ?2",
                    params![if read { 1 } else { 0 }, id],
                )?;
            }
            Fact::Label(label) => {
                let name = label.name.trim();
                if name.is_empty() {
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO labels (item_id, label, by, at, removed) VALUES (?1, ?2, ?3, ?4, 0)
                     ON CONFLICT(item_id, label) DO UPDATE SET by = excluded.by, at = excluded.at, removed = 0",
                    params![id, name, label.by.as_str(), label.at],
                )?;
            }
            Fact::Unlabel(label) => {
                conn.execute(
                    "INSERT INTO labels (item_id, label, by, at, removed) VALUES (?1, ?2, ?3, ?4, 1)
                     ON CONFLICT(item_id, label) DO UPDATE SET by = excluded.by, at = excluded.at, removed = 1",
                    params![id, label.name.trim(), label.by.as_str(), label.at],
                )?;
            }
            Fact::Thread(thread) => {
                let thread = trim_thread(thread.as_deref());
                conn.execute(
                    "UPDATE items SET thread = ?1 WHERE id = ?2",
                    params![thread, id],
                )?;
            }
            Fact::Classified(classifier_id) => {
                conn.execute(
                    "INSERT OR IGNORE INTO llm_classified (item_id, classifier_id) VALUES (?1, ?2)",
                    params![id, classifier_id],
                )?;
            }
            Fact::Vector(vector) => {
                conn.execute(
                    "INSERT OR REPLACE INTO vectors (item_id, data) VALUES (?1, ?2)",
                    params![id, to_blob(&vector)],
                )?;
            }
        }
        Ok(())
    }

    fn blob(&self, part_id: i64) -> Result<Vec<u8>> {
        let conn = self.lock()?;
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT blob FROM parts WHERE id = ?1",
                params![part_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        bytes.with_context(|| format!("part {part_id} has no bytes"))
    }

    fn delete(&self, id: i64) -> Result<bool> {
        let conn = self.lock()?;
        let exists: Option<i64> = conn
            .query_row("SELECT id FROM items WHERE id = ?1", params![id], |row| {
                row.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Ok(false);
        }
        conn.execute("DELETE FROM items_fts WHERE item_id = ?1", params![id])?;
        conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
        Ok(conn.changes() > 0)
    }

    fn classified(&self, id: i64, classifier_id: &str) -> Result<bool> {
        let conn = self.lock()?;
        let hit: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM llm_classified WHERE item_id = ?1 AND classifier_id = ?2",
                params![id, classifier_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hit.is_some())
    }

    fn find(&self, source_id: &str, foreign_id: &str) -> Result<Option<i64>> {
        let conn = self.lock()?;
        let id = conn
            .query_row(
                "SELECT id FROM items WHERE source_id = ?1 AND foreign_id = ?2",
                params![source_id, foreign_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    fn thread(&self, thread: &str) -> Result<Vec<Item>> {
        if thread.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items WHERE thread = ?1 ORDER BY created_at DESC, id DESC"
        ))?;
        let mut items: Vec<Item> = stmt
            .query_map(params![thread], row_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        hydrate(&conn, &mut items)?;
        Ok(items)
    }
}

fn preview_body(item: &NewItem) -> String {
    item.parts
        .iter()
        .find(|p| p.kind == PartKind::Text)
        .and_then(|p| p.text.clone())
        .unwrap_or_else(|| item.body.clone())
}

fn parts_to_insert(item: &NewItem) -> Vec<NewPart> {
    if item.parts.is_empty() && !item.body.is_empty() {
        vec![NewPart {
            kind: PartKind::Text,
            mime: "text/plain".into(),
            text: Some(item.body.clone()),
            bytes: None,
            src: None,
        }]
    } else {
        item.parts.clone()
    }
}

fn trim_thread(thread: Option<&str>) -> Option<String> {
    trim_opt(thread)
}

fn trim_opt(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn actor_cols(actor: Option<&Actor>) -> (Option<String>, Option<String>, Option<String>) {
    match actor {
        Some(a) if !a.id.trim().is_empty() => (
            Some(a.id.clone()),
            a.name.clone().and_then(|n| trim_opt(Some(&n))),
            Some(a.kind.as_str().to_string()),
        ),
        _ => (None, None, None),
    }
}

fn actor_from_cols(
    id: Option<String>,
    name: Option<String>,
    kind: Option<String>,
) -> Option<Actor> {
    let id = id.and_then(|s| trim_opt(Some(&s)))?;
    Some(Actor {
        id,
        name: name.and_then(|s| trim_opt(Some(&s))),
        kind: ActorKind::parse(kind.as_deref().unwrap_or("")),
    })
}

fn insert_to_rows(conn: &Connection, item_id: i64, to: &[Actor]) -> Result<()> {
    for a in to {
        let id = a.id.trim();
        if id.is_empty() {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO item_to (item_id, actor_id, name, kind) VALUES (?1, ?2, ?3, ?4)",
            params![item_id, id, trim_opt(a.name.as_deref()), a.kind.as_str()],
        )?;
    }
    Ok(())
}

fn to_for(conn: &Connection, id: i64) -> Result<Vec<Actor>> {
    let mut stmt = conn
        .prepare("SELECT actor_id, name, kind FROM item_to WHERE item_id = ?1 ORDER BY actor_id")?;
    let rows = stmt.query_map(params![id], |row| {
        Ok(Actor {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: ActorKind::parse(&row.get::<_, String>(2)?),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn insert_part_row(conn: &Connection, item_id: i64, seq: i64, part: &NewPart) -> Result<i64> {
    let bytes = match (&part.bytes, &part.src) {
        (Some(b), _) => Some(b.clone()),
        (None, Some(src)) => Some(std::fs::read(src).with_context(|| format!("read part {src}"))?),
        (None, None) => None,
    };
    conn.execute(
        "INSERT INTO parts (item_id, seq, kind, mime, text, blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            item_id,
            seq,
            part.kind.as_str(),
            part.mime,
            part.text,
            bytes
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        source_id: row.get(1)?,
        foreign_id: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        href: row.get(5)?,
        start: row.get(6)?,
        end: row.get(7)?,
        created_at: row.get(8)?,
        read: row.get::<_, i64>(9)? != 0,
        thread: row.get(10)?,
        from: actor_from_cols(row.get(11)?, row.get(12)?, row.get(13)?),
        in_reply_to: row.get(14)?,
        forward_of: row.get(15)?,
        cite_excerpt: row.get(16)?,
        cite_actor: actor_from_cols(row.get(17)?, row.get(18)?, row.get(19)?),
        labels: Vec::new(),
        denied: Vec::new(),
        parts: Vec::new(),
        to: Vec::new(),
    })
}

/// (live, denied) labels of one item.
fn labels_for(conn: &Connection, id: i64) -> Result<(Vec<Label>, Vec<Label>)> {
    let mut stmt = conn
        .prepare("SELECT label, by, at, removed FROM labels WHERE item_id = ?1 ORDER BY label")?;
    let rows = stmt.query_map(params![id], row_label)?;
    let mut live = Vec::new();
    let mut denied = Vec::new();
    for r in rows {
        let (label, removed) = r?;
        if removed {
            denied.push(label);
        } else {
            live.push(label);
        }
    }
    Ok((live, denied))
}

fn row_label(row: &rusqlite::Row<'_>) -> rusqlite::Result<(Label, bool)> {
    let by: String = row.get(1)?;
    Ok((
        Label {
            name: row.get(0)?,
            by: By::parse(&by),
            at: row.get(2)?,
        },
        row.get::<_, i64>(3)? != 0,
    ))
}

fn parts_for(conn: &Connection, id: i64) -> Result<Vec<Part>> {
    let mut stmt = conn.prepare(
        "SELECT id, seq, kind, mime, text, length(blob) FROM parts WHERE item_id = ?1 ORDER BY seq, id",
    )?;
    let rows = stmt.query_map(params![id], row_part)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn row_part(row: &rusqlite::Row<'_>) -> rusqlite::Result<Part> {
    let kind: String = row.get(2)?;
    Ok(Part {
        id: row.get(0)?,
        seq: row.get(1)?,
        kind: PartKind::parse(&kind),
        mime: row.get(3)?,
        text: row.get(4)?,
        size: row.get(5)?,
    })
}

fn filter_where(filter: &Question) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    match &filter.sources {
        None => {}
        Some(srcs) if srcs.is_empty() => {
            clauses.push("1 = 0".into());
        }
        Some(srcs) => {
            let marks: Vec<&str> = srcs.iter().map(|_| "?").collect();
            clauses.push(format!("source_id IN ({})", marks.join(", ")));
            for s in srcs {
                params.push(Value::Text(s.clone()));
            }
        }
    }
    for label in &filter.labels {
        clauses.push(
            "EXISTS (SELECT 1 FROM labels WHERE labels.item_id = items.id AND labels.label = ? AND labels.removed = 0)"
                .into(),
        );
        params.push(Value::Text(label.clone()));
    }
    if filter.timed {
        clauses.push("start IS NOT NULL AND start != ''".into());
    }
    if filter.unread {
        clauses.push("read = 0".into());
    }
    if let Some(cutoff) = &filter.newer_than {
        clauses.push("COALESCE(NULLIF(start, ''), created_at) >= ?".into());
        params.push(Value::Text(cutoff.clone()));
    }
    if let Some(cutoff) = &filter.older_than {
        clauses.push("COALESCE(NULLIF(start, ''), created_at) < ?".into());
        params.push(Value::Text(cutoff.clone()));
    }
    if let Some(query) = filter.text.as_deref().and_then(fts_query) {
        clauses.push("id IN (SELECT item_id FROM items_fts WHERE items_fts MATCH ?)".into());
        params.push(Value::Text(query));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_sql, params)
}

/// Keep the items that have a vector, closest to `near` first.
fn rank(conn: &Connection, items: Vec<Item>, near: &[f32]) -> Result<Vec<Item>> {
    if items.is_empty() {
        return Ok(items);
    }
    let ids: Vec<Value> = items.iter().map(|i| Value::Integer(i.id)).collect();
    let marks: Vec<&str> = ids.iter().map(|_| "?").collect();
    let mut stmt = conn.prepare(&format!(
        "SELECT item_id, data FROM vectors WHERE item_id IN ({})",
        marks.join(", ")
    ))?;
    let scores: std::collections::HashMap<i64, f32> = stmt
        .query_map(params_from_iter(ids), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                cosine(near, &from_blob(&r.get::<_, Vec<u8>>(1)?)),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut scored: Vec<(f32, Item)> = items
        .into_iter()
        .filter_map(|it| scores.get(&it.id).map(|s| (*s, it)))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored.into_iter().map(|(_, it)| it).collect())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::MIN;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        f32::MIN
    } else {
        dot / (na * nb)
    }
}

fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// One FTS row per item: the title, and every text part (else the body).
const FTS_ROWS: &str = "SELECT i.id, i.title, COALESCE(
        (SELECT group_concat(p.text, ' ') FROM parts p WHERE p.item_id = i.id AND p.text IS NOT NULL),
        i.body) FROM items i";

fn index_text(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM items_fts WHERE item_id = ?1", params![id])?;
    conn.execute(
        &format!("INSERT INTO items_fts(item_id, title, text) {FTS_ROWS} WHERE i.id = ?1"),
        params![id],
    )?;
    Ok(())
}

/// Every word as a quoted prefix term, ANDed: `inv ana` becomes `"inv"* "ana"*`.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn filter_order(filter: &Question) -> &'static str {
    if filter.by_start {
        "ORDER BY CASE WHEN start IS NULL OR start = '' THEN 1 ELSE 0 END, start ASC, id DESC"
    } else {
        "ORDER BY created_at DESC, id DESC"
    }
}

fn hydrate(conn: &Connection, items: &mut [Item]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<Value> = items.iter().map(|i| Value::Integer(i.id)).collect();
    let marks: Vec<&str> = ids.iter().map(|_| "?").collect();
    let in_list = marks.join(", ");

    let mut lab_stmt = conn.prepare(&format!(
        "SELECT item_id, label, by, at, removed FROM labels WHERE item_id IN ({in_list}) ORDER BY label"
    ))?;
    let labs = lab_stmt.query_map(params_from_iter(ids.iter().cloned()), |row| {
        let item_id: i64 = row.get(0)?;
        let by: String = row.get(2)?;
        Ok((
            item_id,
            Label {
                name: row.get(1)?,
                by: By::parse(&by),
                at: row.get(3)?,
            },
            row.get::<_, i64>(4)? != 0,
        ))
    })?;
    let mut lab_map: std::collections::HashMap<i64, (Vec<Label>, Vec<Label>)> =
        std::collections::HashMap::new();
    for row in labs {
        let (id, label, removed) = row?;
        let entry = lab_map.entry(id).or_default();
        if removed {
            entry.1.push(label);
        } else {
            entry.0.push(label);
        }
    }
    drop(lab_stmt);

    let mut part_stmt = conn.prepare(&format!(
        "SELECT id, item_id, seq, kind, mime, text, length(blob) FROM parts WHERE item_id IN ({in_list}) ORDER BY item_id, seq, id"
    ))?;
    let part_rows = part_stmt.query_map(params_from_iter(ids.iter().cloned()), |row| {
        let item_id: i64 = row.get(1)?;
        let kind: String = row.get(3)?;
        Ok((
            item_id,
            Part {
                id: row.get(0)?,
                seq: row.get(2)?,
                kind: PartKind::parse(&kind),
                mime: row.get(4)?,
                text: row.get(5)?,
                size: row.get(6)?,
            },
        ))
    })?;
    let mut part_map: std::collections::HashMap<i64, Vec<Part>> = std::collections::HashMap::new();
    for row in part_rows {
        let (id, part) = row?;
        part_map.entry(id).or_default().push(part);
    }
    drop(part_stmt);

    let mut to_stmt = conn.prepare(&format!(
        "SELECT item_id, actor_id, name, kind FROM item_to WHERE item_id IN ({in_list})"
    ))?;
    let to_rows = to_stmt.query_map(params_from_iter(ids.iter().cloned()), |row| {
        Ok((
            row.get::<_, i64>(0)?,
            Actor {
                id: row.get(1)?,
                name: row.get(2)?,
                kind: ActorKind::parse(&row.get::<_, String>(3)?),
            },
        ))
    })?;
    let mut to_map: std::collections::HashMap<i64, Vec<Actor>> = std::collections::HashMap::new();
    for row in to_rows {
        let (id, actor) = row?;
        to_map.entry(id).or_default().push(actor);
    }
    drop(to_stmt);

    for item in items {
        (item.labels, item.denied) = lab_map.remove(&item.id).unwrap_or_default();
        item.parts = part_map.remove(&item.id).unwrap_or_default();
        item.to = to_map.remove(&item.id).unwrap_or_default();
    }
    Ok(())
}

fn ensure_text_part(conn: &Connection, item: &mut Item) -> Result<()> {
    if item.parts.is_empty() && !item.body.is_empty() {
        conn.execute(
            "INSERT INTO parts (item_id, seq, kind, mime, text, blob)
             VALUES (?1, 0, 'text', 'text/plain', ?2, NULL)",
            params![item.id, item.body],
        )?;
        item.parts = parts_for(conn, item.id)?;
    }
    Ok(())
}

fn backfill_parts(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO parts (item_id, seq, kind, mime, text, blob)
         SELECT id, 0, 'text', 'text/plain', body, NULL
         FROM items
         WHERE body != ''
           AND NOT EXISTS (SELECT 1 FROM parts WHERE parts.item_id = items.id)",
        [],
    )?;
    Ok(())
}

fn lookup_id(conn: &Connection, source_id: &str, foreign_id: &str) -> Result<Option<i64>> {
    let id = conn
        .query_row(
            "SELECT id FROM items WHERE source_id = ?1 AND foreign_id = ?2",
            params![source_id, foreign_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(id)
}

fn stitch_cites(conn: &Connection, source_id: &str, foreign_id: &str, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE items SET in_reply_to = ?1
         WHERE source_id = ?2 AND in_reply_to_foreign = ?3",
        params![id, source_id, foreign_id],
    )?;
    conn.execute(
        "UPDATE items SET forward_of = ?1
         WHERE source_id = ?2 AND forward_of_foreign = ?3",
        params![id, source_id, foreign_id],
    )?;
    Ok(())
}

fn ensure_column(conn: &Connection, table: &str, name: &str, decl: &str) -> Result<()> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|n| n == name);
    drop(stmt);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"), [])?;
    }
    Ok(())
}

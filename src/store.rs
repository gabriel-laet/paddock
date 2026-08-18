use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::source::NewItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PartKind {
    #[default]
    Text,
    File,
    Image,
    Audio,
    Video,
}

impl PartKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::File => "file",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "text" => Self::Text,
            "file" => Self::File,
            "image" => Self::Image,
            "audio" => Self::Audio,
            "video" => Self::Video,
            _ => Self::File,
        }
    }

    pub fn is_media(self) -> bool {
        matches!(self, Self::Image | Self::Audio | Self::Video)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActorKind {
    #[default]
    Person,
    Group,
    List,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Group => "group",
            Self::List => "list",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "group" => Self::Group,
            "list" => Self::List,
            _ => Self::Person,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Actor {
    pub id: String,
    pub name: Option<String>,
    pub kind: ActorKind,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub id: i64,
    pub seq: i64,
    pub kind: PartKind,
    pub mime: String,
    pub text: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewPart {
    pub kind: PartKind,
    pub mime: String,
    pub text: Option<String>,
    pub bytes: Option<Vec<u8>>,
    pub src: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Item {
    pub id: i64,
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub thread: Option<String>,
    pub created_at: String,
    pub read: bool,
    pub labels: Vec<String>,
    pub parts: Vec<Part>,
    pub from: Option<Actor>,
    pub to: Vec<Actor>,
    pub in_reply_to: Option<i64>,
    pub forward_of: Option<i64>,
    pub cite_excerpt: Option<String>,
    pub cite_actor: Option<Actor>,
}

const ITEM_COLS: &str =
    "id, source_id, foreign_id, title, body, href, start, end, created_at, read, thread,      from_id, from_name, from_kind, in_reply_to, forward_of, cite_excerpt,      cite_actor_id, cite_actor_name, cite_actor_kind";

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    data_dir: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let conn = Connection::open(path)
            .with_context(|| format!("open {}", path.display()))?;
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
                path TEXT,
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
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_items_thread ON items(thread);
             CREATE INDEX IF NOT EXISTS idx_items_reply_foreign ON items(source_id, in_reply_to_foreign);
             CREATE INDEX IF NOT EXISTS idx_items_fwd_foreign ON items(source_id, forward_of_foreign);",
        )?;
        backfill_parts(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("store lock: {e}"))
    }

    /// Insert if new. Returns Some(id) when a row was created.
    pub fn insert_new(&self, item: &NewItem) -> Result<Option<i64>> {
        let (id, created) = self.upsert(item)?;
        if created {
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Insert or refresh on `(source_id, foreign_id)`. The bool is true when created.
    pub fn upsert(&self, item: &NewItem) -> Result<(i64, bool)> {
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
                    insert_part_row(&tx, &self.data_dir, id, seq as i64, part)?;
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
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
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
                insert_part_row(&tx, &self.data_dir, id, seq as i64, part)?;
            }
            insert_to_rows(&tx, id, &item.to)?;
            (id, true)
        };
        stitch_cites(&tx, &item.source_id, &item.foreign_id, id)?;
        tx.commit()?;
        Ok((id, created))
    }

    pub fn update_body(&self, source_id: &str, foreign_id: &str, title: &str, body: &str, href: Option<&str>) -> Result<bool> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE items SET title = ?1, body = ?2, href = ?3
             WHERE source_id = ?4 AND foreign_id = ?5",
            params![title, body, href, source_id, foreign_id],
        )?;
        Ok(conn.changes() > 0)
    }

    pub fn get(&self, id: i64) -> Result<Item> {
        let conn = self.lock()?;
        let mut item = conn.query_row(
            &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
            params![id],
            row_item,
        )?;
        item.labels = labels_for(&conn, id)?;
        item.parts = parts_for(&conn, id)?;
        item.to = to_for(&conn, id)?;
        ensure_text_part(&conn, &mut item)?;
        Ok(item)
    }

    pub fn list_all(&self) -> Result<Vec<Item>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_COLS} FROM items ORDER BY created_at DESC, id DESC"
        ))?;
        let mut items: Vec<Item> = stmt
            .query_map([], row_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        hydrate(&conn, &mut items)?;
        Ok(items)
    }

    pub fn set_read(&self, id: i64, read: bool) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE items SET read = ?1 WHERE id = ?2",
            params![if read { 1 } else { 0 }, id],
        )?;
        Ok(())
    }

    pub fn toggle_read(&self, id: i64) -> Result<bool> {
        let mut item = self.get(id)?;
        item.read = !item.read;
        self.set_read(id, item.read)?;
        Ok(item.read)
    }

    pub fn add_label(&self, id: i64, label: &str) -> Result<()> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(());
        }
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO labels (item_id, label) VALUES (?1, ?2)",
            params![id, label],
        )?;
        Ok(())
    }

    pub fn remove_label(&self, id: i64, label: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM labels WHERE item_id = ?1 AND label = ?2",
            params![id, label],
        )?;
        Ok(())
    }

    pub fn toggle_label(&self, id: i64, label: &str) -> Result<bool> {
        let label = label.trim();
        if label.is_empty() {
            return Ok(false);
        }
        let item = self.get(id)?;
        if item.labels.iter().any(|l| l == label) {
            self.remove_label(id, label)?;
            Ok(false)
        } else {
            self.add_label(id, label)?;
            Ok(true)
        }
    }

    pub fn id_by_foreign(&self, source_id: &str, foreign_id: &str) -> Result<Option<i64>> {
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

    pub fn add_part(&self, item_id: i64, part: &NewPart) -> Result<i64> {
        let conn = self.lock()?;
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), -1) + 1 FROM parts WHERE item_id = ?1",
            params![item_id],
            |row| row.get(0),
        )?;
        insert_part_row(&conn, &self.data_dir, item_id, seq, part)
    }

    pub fn set_thread(&self, item_id: i64, thread: Option<&str>) -> Result<()> {
        let thread = trim_thread(thread);
        let conn = self.lock()?;
        conn.execute(
            "UPDATE items SET thread = ?1 WHERE id = ?2",
            params![thread, item_id],
        )?;
        Ok(())
    }

    pub fn set_in_reply_to(&self, item_id: i64, parent: Option<i64>) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE items SET in_reply_to = ?1 WHERE id = ?2",
            params![parent, item_id],
        )?;
        Ok(())
    }

    pub fn set_to(&self, item_id: i64, to: &[Actor]) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM item_to WHERE item_id = ?1", params![item_id])?;
        insert_to_rows(&conn, item_id, to)?;
        Ok(())
    }

    pub fn get_part(&self, id: i64) -> Result<Part> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT id, seq, kind, mime, text, path FROM parts WHERE id = ?1",
            params![id],
            row_part,
        )
        .with_context(|| format!("part {id}"))
    }

    pub fn part_abs_path(&self, part: &Part) -> Option<PathBuf> {
        let p = part.path.as_ref()?;
        let path = Path::new(p);
        if path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            Some(self.data_dir.join(path))
        }
    }

    pub fn items_in_thread(&self, thread: &str) -> Result<Vec<Item>> {
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

fn actor_from_cols(id: Option<String>, name: Option<String>, kind: Option<String>) -> Option<Actor> {
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
    let mut stmt = conn.prepare(
        "SELECT actor_id, name, kind FROM item_to WHERE item_id = ?1 ORDER BY actor_id",
    )?;
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

fn insert_part_row(
    conn: &Connection,
    data_dir: &Path,
    item_id: i64,
    seq: i64,
    part: &NewPart,
) -> Result<i64> {
    let path = materialize_part_path(data_dir, item_id, seq, part)?;
    conn.execute(
        "INSERT INTO parts (item_id, seq, kind, mime, text, path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            item_id,
            seq,
            part.kind.as_str(),
            part.mime,
            part.text,
            path
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn materialize_part_path(
    data_dir: &Path,
    item_id: i64,
    seq: i64,
    part: &NewPart,
) -> Result<Option<String>> {
    let bytes = if let Some(b) = &part.bytes {
        Some(b.clone())
    } else if let Some(src) = &part.src {
        Some(std::fs::read(src).with_context(|| format!("read part {src}"))?)
    } else {
        None
    };
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let ext = extension_for(part);
    let dir = data_dir.join("parts");
    std::fs::create_dir_all(&dir)?;
    let name = format!("{item_id}-{seq}{ext}");
    std::fs::write(dir.join(&name), bytes)?;
    Ok(Some(format!("parts/{name}")))
}

fn extension_for(part: &NewPart) -> String {
    if let Some(src) = &part.src {
        if let Some(e) = Path::new(src).extension().and_then(|s| s.to_str()) {
            if !e.is_empty() {
                return format!(".{e}");
            }
        }
    }
    match part.kind {
        PartKind::Image => {
            if part.mime.contains("png") {
                ".png".into()
            } else if part.mime.contains("jpeg") || part.mime.contains("jpg") {
                ".jpg".into()
            } else if part.mime.contains("gif") {
                ".gif".into()
            } else if part.mime.contains("webp") {
                ".webp".into()
            } else {
                ".bin".into()
            }
        }
        PartKind::Audio => {
            if part.mime.contains("mpeg") || part.mime.contains("mp3") {
                ".mp3".into()
            } else if part.mime.contains("wav") {
                ".wav".into()
            } else if part.mime.contains("ogg") {
                ".ogg".into()
            } else if part.mime.contains("mp4") || part.mime.contains("m4a") {
                ".m4a".into()
            } else {
                ".bin".into()
            }
        }
        PartKind::Video => {
            if part.mime.contains("webm") {
                ".webm".into()
            } else if part.mime.contains("quicktime") {
                ".mov".into()
            } else if part.mime.contains("matroska") {
                ".mkv".into()
            } else {
                ".mp4".into()
            }
        }
        _ => String::new(),
    }
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
        parts: Vec::new(),
        to: Vec::new(),
    })
}

fn labels_for(conn: &Connection, id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT label FROM labels WHERE item_id = ?1 ORDER BY label")?;
    let rows = stmt.query_map(params![id], |row| row.get(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn parts_for(conn: &Connection, id: i64) -> Result<Vec<Part>> {
    let mut stmt = conn.prepare(
        "SELECT id, seq, kind, mime, text, path FROM parts WHERE item_id = ?1 ORDER BY seq, id",
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
        path: row.get(5)?,
    })
}

fn hydrate(conn: &Connection, items: &mut [Item]) -> Result<()> {
    let mut lab_stmt = conn.prepare("SELECT item_id, label FROM labels")?;
    let labs = lab_stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut lab_map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
    for row in labs {
        let (id, label) = row?;
        lab_map.entry(id).or_default().push(label);
    }
    drop(lab_stmt);

    let mut part_stmt = conn.prepare(
        "SELECT id, item_id, seq, kind, mime, text, path FROM parts ORDER BY item_id, seq, id",
    )?;
    let part_rows = part_stmt.query_map([], |row| {
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
                path: row.get(6)?,
            },
        ))
    })?;
    let mut part_map: std::collections::HashMap<i64, Vec<Part>> = std::collections::HashMap::new();
    for row in part_rows {
        let (id, part) = row?;
        part_map.entry(id).or_default().push(part);
    }
    drop(part_stmt);

    let mut to_stmt = conn.prepare("SELECT item_id, actor_id, name, kind FROM item_to")?;
    let to_rows = to_stmt.query_map([], |row| {
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
        item.labels = lab_map.remove(&item.id).unwrap_or_default();
        item.parts = part_map.remove(&item.id).unwrap_or_default();
        item.to = to_map.remove(&item.id).unwrap_or_default();
    }
    Ok(())
}

fn ensure_text_part(conn: &Connection, item: &mut Item) -> Result<()> {
    if item.parts.is_empty() && !item.body.is_empty() {
        conn.execute(
            "INSERT INTO parts (item_id, seq, kind, mime, text, path)
             VALUES (?1, 0, 'text', 'text/plain', ?2, NULL)",
            params![item.id, item.body],
        )?;
        item.parts = parts_for(conn, item.id)?;
    }
    Ok(())
}

fn backfill_parts(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO parts (item_id, seq, kind, mime, text, path)
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

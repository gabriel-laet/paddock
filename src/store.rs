use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::source::NewItem;

#[derive(Debug, Clone)]
pub struct Item {
    pub id: i64,
    pub source_id: String,
    pub foreign_id: String,
    pub title: String,
    pub body: String,
    pub href: Option<String>,
    pub created_at: String,
    pub read: bool,
    pub labels: Vec<String>,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
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
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|e| anyhow::anyhow!("store lock: {e}"))
    }

    /// Insert if new. Returns Some(id) when a row was created.
    pub fn insert_new(&self, item: &NewItem) -> Result<Option<i64>> {
        let created = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO items
                (source_id, foreign_id, title, body, href, created_at, read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![
                item.source_id,
                item.foreign_id,
                item.title,
                item.body,
                item.href,
                created
            ],
        )?;
        if conn.changes() == 0 {
            return Ok(None);
        }
        Ok(Some(conn.last_insert_rowid()))
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
            "SELECT id, source_id, foreign_id, title, body, href, created_at, read
             FROM items WHERE id = ?1",
            params![id],
            row_item,
        )?;
        item.labels = labels_for(&conn, id)?;
        Ok(item)
    }

    pub fn list_all(&self) -> Result<Vec<Item>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, source_id, foreign_id, title, body, href, created_at, read
             FROM items ORDER BY created_at DESC, id DESC",
        )?;
        let mut items: Vec<Item> = stmt
            .query_map([], row_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut lab_stmt = conn.prepare("SELECT item_id, label FROM labels")?;
        let labs = lab_stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map: std::collections::HashMap<i64, Vec<String>> = std::collections::HashMap::new();
        for row in labs {
            let (id, label) = row?;
            map.entry(id).or_default().push(label);
        }
        for item in &mut items {
            item.labels = map.remove(&item.id).unwrap_or_default();
        }
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
}

fn row_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    Ok(Item {
        id: row.get(0)?,
        source_id: row.get(1)?,
        foreign_id: row.get(2)?,
        title: row.get(3)?,
        body: row.get(4)?,
        href: row.get(5)?,
        created_at: row.get(6)?,
        read: row.get::<_, i64>(7)? != 0,
        labels: Vec::new(),
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

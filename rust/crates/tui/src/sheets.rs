use std::sync::{Arc, Mutex};

use cordanui_schema::GoalSheet;
use cordanui_sync::Database;
use cordanui_plugin_runtime::ui::SheetsHost;

/// Host for `cord.sheets` — sheets (buffers) for work/project separation.
/// Backed by `goal_sheets` table, synced via Turso. Active sheet is shared
/// with `App` via `Arc<Mutex<Option<String>>>` so plugin worker threads and
/// the UI thread see the same selection.
pub struct SheetManager {
    db: Mutex<Option<Database>>,
    active: Arc<Mutex<Option<String>>>,
}

impl SheetManager {
    pub fn new(active: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            db: Mutex::new(None),
            active,
        }
    }

    pub fn attach_db(&self, db: Database) {
        *self.db.lock().unwrap() = Some(db);
    }
}

impl SheetsHost for SheetManager {
    fn list_sheets(&self) -> Vec<GoalSheet> {
        if let Some(db) = self.db.lock().unwrap().as_ref() {
            crate::db::list_sheets(db).unwrap_or_default()
        } else {
            Vec::new()
        }
    }

    fn create_sheet(&self, name: &str) -> anyhow::Result<String> {
        let db_guard = self.db.lock().unwrap();
        let Some(db) = db_guard.as_ref() else {
            anyhow::bail!("sheets: no db attached");
        };
        let sheet = crate::db::create_sheet(db, name)?;
        Ok(sheet.id)
    }

    fn delete_sheet(&self, id: &str) -> anyhow::Result<()> {
        let db_guard = self.db.lock().unwrap();
        let Some(db) = db_guard.as_ref() else {
            anyhow::bail!("sheets: no db attached");
        };
        crate::db::delete_sheet(db, id)?;
        // Orphan goals in that sheet to All (sheet_id = NULL) and mark dirty.
        let _ = db.execute(
            "UPDATE goals SET sheet_id = NULL, updated_at = ? WHERE sheet_id = ?",
            vec![
                cordanui_sync::Value::from(cordanui_schema::now_iso()),
                cordanui_sync::Value::from(id),
            ],
        );
        // Mark each orphaned goal dirty for sync.
        if let Ok(rows) = db.query("SELECT id FROM goals WHERE sheet_id IS NULL AND updated_at = ?", vec![cordanui_sync::Value::from(cordanui_schema::now_iso())]) {
            for row in rows.rows() {
                if let Some(cordanui_sync::Value::Text(gid)) = row.first() {
                    let _ = db.mark_dirty("goals", gid);
                }
            }
        }
        // If the deleted sheet was active, clear selection.
        let mut active = self.active.lock().unwrap();
        if active.as_deref() == Some(id) {
            *active = None;
        }
        Ok(())
    }

    fn select_sheet(&self, id: Option<String>) -> anyhow::Result<()> {
        // Validate id exists if Some.
        if let Some(ref sid) = id {
            let db_guard = self.db.lock().unwrap();
            if let Some(db) = db_guard.as_ref() {
                let exists = db
                    .query("SELECT 1 FROM goal_sheets WHERE id = ? AND deleted_at IS NULL", vec![cordanui_sync::Value::from(sid.clone())])
                    .map(|r| !r.rows().is_empty())
                    .unwrap_or(false);
                if !exists {
                    anyhow::bail!("sheet not found: {}", sid);
                }
            }
        }
        *self.active.lock().unwrap() = id;
        Ok(())
    }

    fn current_sheet(&self) -> Option<String> {
        self.active.lock().unwrap().clone()
    }
}

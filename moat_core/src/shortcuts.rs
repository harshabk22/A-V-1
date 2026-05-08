use crate::execution_plan::ExecutionPlan;
use crate::{MoatError, MoatResult};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutRecord {
    pub trigger_name: String,
    pub workflow_json: String,
    pub created_at: String,
}

pub fn prepare_shortcut_table(conn: &Connection) -> MoatResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS shortcut_workflows (
            trigger_name TEXT PRIMARY KEY,
            workflow_json TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
        [],
    )
    .map_err(MoatError::Database)?;
    migrate_legacy_shortcuts(conn)?;
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> MoatResult<bool> {
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 LIMIT 1",
            params![table],
            |_row| Ok(()),
        )
        .optional()
        .map_err(MoatError::Database)?
        .is_some();
    Ok(exists)
}

fn migrate_legacy_shortcuts(conn: &Connection) -> MoatResult<()> {
    if !table_exists(conn, "mca_shortcuts")? {
        return Ok(());
    }

    conn.execute(
        "INSERT OR IGNORE INTO shortcut_workflows (trigger_name, workflow_json, created_at)
         SELECT trigger_name, dag_json, created_at
         FROM mca_shortcuts",
        [],
    )
    .map_err(MoatError::Database)?;
    conn.execute("DROP TABLE IF EXISTS mca_shortcuts", [])
        .map_err(MoatError::Database)?;
    Ok(())
}

pub fn normalize_trigger_name(trigger_name: &str) -> MoatResult<String> {
    let trimmed = trigger_name.trim();
    if trimmed.is_empty() {
        return Err(MoatError::Settings(
            "shortcut trigger_name cannot be empty".into(),
        ));
    }
    if !trimmed.starts_with('/') {
        return Err(MoatError::Settings(
            "shortcut trigger_name must start with '/'".into(),
        ));
    }
    Ok(trimmed.to_string())
}

pub fn save_shortcut(conn: &Connection, trigger_name: &str, workflow_json: &str) -> MoatResult<()> {
    let trigger_name = normalize_trigger_name(trigger_name)?;
    conn.execute(
        "INSERT OR REPLACE INTO shortcut_workflows (trigger_name, workflow_json, created_at)
         VALUES (?1, ?2, ?3)",
        params![trigger_name, workflow_json, Utc::now().to_rfc3339()],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn list_shortcuts(conn: &Connection) -> MoatResult<Vec<ShortcutRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT trigger_name, workflow_json, created_at
             FROM shortcut_workflows
             ORDER BY trigger_name ASC",
        )
        .map_err(MoatError::Database)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ShortcutRecord {
                trigger_name: row.get(0)?,
                workflow_json: row.get(1)?,
                created_at: row.get(2)?,
            })
        })
        .map_err(MoatError::Database)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MoatError::Database)
}

pub fn delete_shortcut(conn: &Connection, trigger_name: &str) -> MoatResult<()> {
    let trigger_name = normalize_trigger_name(trigger_name)?;
    conn.execute(
        "DELETE FROM shortcut_workflows WHERE trigger_name = ?1",
        params![trigger_name],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn load_shortcut(conn: &Connection, trigger_name: &str) -> MoatResult<Option<ShortcutRecord>> {
    let trigger_name = normalize_trigger_name(trigger_name)?;
    let mut stmt = conn
        .prepare(
            "SELECT trigger_name, workflow_json, created_at
             FROM shortcut_workflows
             WHERE trigger_name = ?1",
        )
        .map_err(MoatError::Database)?;
    let mut rows = stmt
        .query(params![trigger_name])
        .map_err(MoatError::Database)?;
    let Some(row) = rows.next().map_err(MoatError::Database)? else {
        return Ok(None);
    };
    Ok(Some(ShortcutRecord {
        trigger_name: row.get(0).map_err(MoatError::Database)?,
        workflow_json: row.get(1).map_err(MoatError::Database)?,
        created_at: row.get(2).map_err(MoatError::Database)?,
    }))
}

pub fn load_shortcut_plan(
    conn: &Connection,
    trigger_name: &str,
) -> MoatResult<Option<ExecutionPlan>> {
    let Some(record) = load_shortcut(conn, trigger_name)? else {
        return Ok(None);
    };
    let plan = serde_json::from_str::<ExecutionPlan>(&record.workflow_json).map_err(|err| {
        MoatError::Settings(format!(
            "shortcut '{}' contains invalid workflow JSON: {err}",
            record.trigger_name
        ))
    })?;
    Ok(Some(plan))
}

pub fn materialize_runtime_plan(mut plan: ExecutionPlan) -> ExecutionPlan {
    plan.workflow_id = Uuid::new_v4();
    plan.session_id = Uuid::new_v4();
    plan
}

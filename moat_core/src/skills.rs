use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config;
use crate::{MoatError, MoatResult};

pub const NATIVE_TOOL_NAMES: &[&str] = &[
    "fs:read",
    "fs:write",
    "app:launch",
    "process:spawn",
    "os:open_url",
    "os:applescript",
    "os:inject_text",
    "net:curl",
    "memory:search",
    "memory:learn",
    "meta:query_brain",
    "request_sub_agent",
    "swarm:delegate",
    "propose_system_skill",
    "user:ask_choice",
    "done",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SynthesizedSkill {
    pub skill_name: String,
    pub description: String,
    pub script_path: String,
    pub code_hash: String,
    pub language: String,
    pub parameter_schema: String,
    pub created_at: String,
}

impl SynthesizedSkill {
    pub fn parsed_parameter_schema(&self) -> MoatResult<Value> {
        serde_json::from_str(&self.parameter_schema).map_err(MoatError::Serde)
    }
}

#[derive(Debug, Clone)]
pub struct SkillProposal {
    pub skill_name: String,
    pub description: String,
    pub source_code: String,
    pub language: String,
    pub parameter_schema: String,
}

pub fn prepare_skills_table(conn: &Connection) -> MoatResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS synthesized_skills (
            skill_name TEXT PRIMARY KEY,
            description TEXT NOT NULL,
            script_path TEXT NOT NULL,
            code_hash TEXT NOT NULL,
            language TEXT NOT NULL,
            parameter_schema TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
        [],
    )
    .map_err(MoatError::Database)?;

    let mut has_parameter_schema = false;
    let mut stmt = conn
        .prepare("PRAGMA table_info(synthesized_skills)")
        .map_err(MoatError::Database)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(MoatError::Database)?;
    for row in rows {
        if row.map_err(MoatError::Database)? == "parameter_schema" {
            has_parameter_schema = true;
            break;
        }
    }

    if !has_parameter_schema {
        conn.execute(
            "ALTER TABLE synthesized_skills ADD COLUMN parameter_schema TEXT NOT NULL DEFAULT '{}'",
            [],
        )
        .map_err(MoatError::Database)?;
    }

    Ok(())
}

pub fn ensure_skills_vault() -> MoatResult<PathBuf> {
    let root = config::skills_root();
    fs::create_dir_all(&root)
        .map_err(|err| MoatError::Settings(format!("failed to create skills vault: {err}")))?;
    Ok(root)
}

pub fn list_skills(conn: &Connection) -> MoatResult<Vec<SynthesizedSkill>> {
    let mut stmt = match conn.prepare(
        "SELECT skill_name, description, script_path, code_hash, language, parameter_schema, created_at
         FROM synthesized_skills
         ORDER BY created_at ASC, skill_name ASC",
    ) {
        Ok(stmt) => stmt,
        Err(err) if is_missing_skills_table(&err) => return Ok(Vec::new()),
        Err(err) => return Err(MoatError::Database(err)),
    };
    let rows = stmt
        .query_map([], |row| {
            Ok(SynthesizedSkill {
                skill_name: row.get(0)?,
                description: row.get(1)?,
                script_path: row.get(2)?,
                code_hash: row.get(3)?,
                language: row.get(4)?,
                parameter_schema: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(MoatError::Database)?;

    let mut skills = Vec::new();
    for row in rows {
        skills.push(row.map_err(MoatError::Database)?);
    }
    Ok(skills)
}

pub fn get_skill(conn: &Connection, skill_name: &str) -> MoatResult<Option<SynthesizedSkill>> {
    match conn.query_row(
        "SELECT skill_name, description, script_path, code_hash, language, parameter_schema, created_at
         FROM synthesized_skills
         WHERE skill_name = ?1",
        params![skill_name.trim()],
        |row| {
            Ok(SynthesizedSkill {
                skill_name: row.get(0)?,
                description: row.get(1)?,
                script_path: row.get(2)?,
                code_hash: row.get(3)?,
                language: row.get(4)?,
                parameter_schema: row.get(5)?,
                created_at: row.get(6)?,
            })
        },
    ) {
        Ok(skill) => Ok(Some(skill)),
        Err(err) if is_missing_skills_table(&err) => Ok(None),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(err) => Err(MoatError::Database(err)),
    }
}

pub fn install_skill(conn: &Connection, proposal: &SkillProposal) -> MoatResult<SynthesizedSkill> {
    let skill_name = normalize_skill_name(&proposal.skill_name)?;
    if native_tool_name_collision(&skill_name) {
        return Err(MoatError::InvariantViolation(format!(
            "skill name '{}' collides with a built-in tool",
            skill_name
        )));
    }
    if get_skill(conn, &skill_name)?.is_some() {
        return Err(MoatError::InvariantViolation(format!(
            "skill '{}' is already installed",
            skill_name
        )));
    }

    let language = normalize_language(&proposal.language)?;
    let parameter_schema = normalize_parameter_schema(&proposal.parameter_schema)?;
    let description = proposal.description.trim();
    let source_code = proposal.source_code.as_bytes();
    if description.is_empty() {
        return Err(MoatError::Orchestrator(
            "skill description cannot be empty".into(),
        ));
    }
    if source_code.is_empty() {
        return Err(MoatError::Orchestrator(
            "skill source code cannot be empty".into(),
        ));
    }

    let vault_root = ensure_skills_vault()?;
    let extension = language_extension(&language);
    let script_path = vault_root.join(format!("{skill_name}.{extension}"));
    let tmp_path = vault_root.join(format!("{skill_name}.{extension}.tmp"));
    let code_hash = sha256_hex(source_code);
    let created_at = Utc::now().to_rfc3339();

    fs::write(&tmp_path, source_code)
        .map_err(|err| MoatError::Settings(format!("failed to write temp skill file: {err}")))?;
    if let Err(err) = fs::rename(&tmp_path, &script_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(MoatError::Settings(format!(
            "failed to finalize skill install: {err}"
        )));
    }

    if let Err(err) = set_vault_readonly(&script_path) {
        return Err(MoatError::Settings(format!(
            "failed to lock installed skill file: {err}"
        )));
    }

    let record = SynthesizedSkill {
        skill_name,
        description: description.to_string(),
        script_path: script_path.to_string_lossy().into_owned(),
        code_hash,
        language,
        parameter_schema,
        created_at,
    };

    if let Err(err) = conn.execute(
        "INSERT INTO synthesized_skills (
            skill_name, description, script_path, code_hash, language, parameter_schema, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            record.skill_name,
            record.description,
            record.script_path,
            record.code_hash,
            record.language,
            record.parameter_schema,
            record.created_at,
        ],
    ) {
        let _ = fs::remove_file(&record.script_path);
        return Err(MoatError::Database(err));
    }

    Ok(record)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub fn native_tool_name_collision(name: &str) -> bool {
    NATIVE_TOOL_NAMES
        .iter()
        .any(|native| native.eq_ignore_ascii_case(name.trim()))
}

pub fn normalize_skill_name(raw: &str) -> MoatResult<String> {
    let candidate = raw.trim();
    if candidate.is_empty() {
        return Err(MoatError::Orchestrator("skill_name cannot be empty".into()));
    }
    if !candidate
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
    {
        return Err(MoatError::Orchestrator(
            "skill_name must use lowercase letters, digits, '_' or '-'".into(),
        ));
    }
    Ok(candidate.to_string())
}

pub fn normalize_language(raw: &str) -> MoatResult<String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "python" => Ok("python".to_string()),
        "bash" => Ok("bash".to_string()),
        other => Err(MoatError::Orchestrator(format!(
            "unsupported skill language '{}'; expected 'python' or 'bash'",
            other
        ))),
    }
}

pub fn language_extension(language: &str) -> &'static str {
    match language {
        "python" => "py",
        "bash" => "sh",
        _ => "txt",
    }
}

pub fn interpreter_for_language(language: &str) -> &'static str {
    match language {
        "python" => "python3",
        "bash" => "bash",
        _ => "bash",
    }
}

pub fn normalize_parameter_schema(raw: &str) -> MoatResult<String> {
    let value: Value = serde_json::from_str(raw).map_err(MoatError::Serde)?;
    if !value.is_object() {
        return Err(MoatError::Orchestrator(
            "skill parameters must be a JSON object schema".into(),
        ));
    }
    serde_json::to_string(&value).map_err(MoatError::Serde)
}

#[cfg(unix)]
fn set_vault_readonly(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o555);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_vault_readonly(path: &Path) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)
}

fn is_missing_skills_table(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => {
            message.contains("no such table: synthesized_skills")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_skill_name_rejects_invalid_characters() {
        assert!(normalize_skill_name("valid_skill-1").is_ok());
        assert!(normalize_skill_name("Invalid Skill").is_err());
        assert!(normalize_skill_name("../escape").is_err());
    }

    #[test]
    fn normalize_parameter_schema_requires_object() {
        assert!(normalize_parameter_schema("{\"url\":\"string\"}").is_ok());
        assert!(normalize_parameter_schema("[\"bad\"]").is_err());
    }
}

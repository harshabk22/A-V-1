use crate::shortcuts::normalize_trigger_name;
use crate::{MoatError, MoatResult};
use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

const FILE_EVENT_DEDUPE_MS: u64 = 2_000;

type TriggerHandler = Arc<dyn Fn(String) + Send + Sync>;
type FaultHandler = Arc<dyn Fn(SentinelFault) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SentinelTriggerType {
    Cron,
    FileWatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SentinelRuleRecord {
    pub rule_id: String,
    pub trigger_type: SentinelTriggerType,
    pub trigger_config: String,
    pub target_shortcut: String,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct SentinelFault {
    pub severity: String,
    pub subsystem: String,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Default)]
struct SentinelRuntime {
    scheduler: Option<JobScheduler>,
    watchers: Vec<RecommendedWatcher>,
    trigger_handler: Option<TriggerHandler>,
    fault_handler: Option<FaultHandler>,
}

pub struct SentinelDaemon {
    db: Arc<AsyncMutex<Connection>>,
    runtime: AsyncMutex<SentinelRuntime>,
}

impl SentinelDaemon {
    pub fn new(db: Arc<AsyncMutex<Connection>>) -> Self {
        Self {
            db,
            runtime: AsyncMutex::new(SentinelRuntime::default()),
        }
    }

    pub async fn start(
        &self,
        trigger_handler: TriggerHandler,
        fault_handler: FaultHandler,
    ) -> MoatResult<()> {
        {
            let mut runtime = self.runtime.lock().await;
            runtime.trigger_handler = Some(trigger_handler);
            runtime.fault_handler = Some(fault_handler);
        }
        self.reload().await
    }

    pub async fn reload(&self) -> MoatResult<()> {
        let rules = {
            let db = self.db.lock().await;
            list_sentinel_rules(&db)?
                .into_iter()
                .filter(|rule| rule.is_active)
                .collect::<Vec<_>>()
        };

        let (trigger_handler, fault_handler, old_scheduler) = {
            let mut runtime = self.runtime.lock().await;
            let trigger = runtime.trigger_handler.clone();
            let fault = runtime.fault_handler.clone();
            let scheduler = runtime.scheduler.take();
            runtime.watchers.clear();
            (trigger, fault, scheduler)
        };

        if let Some(mut scheduler) = old_scheduler {
            let _ = scheduler.shutdown().await;
        }

        let Some(trigger_handler) = trigger_handler else {
            return Ok(());
        };
        let Some(fault_handler) = fault_handler else {
            return Ok(());
        };

        let scheduler = JobScheduler::new().await.map_err(|err| {
            MoatError::Orchestrator(format!("failed to start sentinel scheduler: {err}"))
        })?;
        let mut watchers = Vec::new();

        for rule in rules {
            match rule.trigger_type {
                SentinelTriggerType::Cron => {
                    if let Err(err) =
                        install_cron_rule(&scheduler, &rule, Arc::clone(&trigger_handler)).await
                    {
                        fault_handler(SentinelFault {
                            severity: "error".into(),
                            subsystem: "sentinel".into(),
                            message: err.to_string(),
                            code: Some("SENTINEL_CRON_INSTALL_FAILED".into()),
                        });
                    }
                }
                SentinelTriggerType::FileWatch => match install_file_watch_rule(
                    &rule,
                    Arc::clone(&trigger_handler),
                    Arc::clone(&fault_handler),
                ) {
                    Ok(watcher) => watchers.push(watcher),
                    Err(err) => fault_handler(SentinelFault {
                        severity: "error".into(),
                        subsystem: "sentinel".into(),
                        message: err.to_string(),
                        code: Some("SENTINEL_FILEWATCH_INSTALL_FAILED".into()),
                    }),
                },
            }
        }

        scheduler.start().await.map_err(|err| {
            MoatError::Orchestrator(format!("failed to start sentinel jobs: {err}"))
        })?;

        let mut runtime = self.runtime.lock().await;
        runtime.scheduler = Some(scheduler);
        runtime.watchers = watchers;
        Ok(())
    }
}

pub fn prepare_sentinel_table(conn: &Connection) -> MoatResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sentinel_rules (
            rule_id TEXT PRIMARY KEY,
            trigger_type TEXT NOT NULL,
            trigger_config TEXT NOT NULL,
            target_shortcut TEXT NOT NULL,
            is_active BOOLEAN DEFAULT TRUE
        )",
        [],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn add_sentinel_rule(
    conn: &Connection,
    trigger_type: SentinelTriggerType,
    trigger_config: &str,
    target_shortcut: &str,
) -> MoatResult<SentinelRuleRecord> {
    let trigger_config = trigger_config.trim();
    if trigger_config.is_empty() {
        return Err(MoatError::Settings(
            "sentinel trigger_config cannot be empty".into(),
        ));
    }
    let target_shortcut = normalize_trigger_name(target_shortcut)?;
    let record = SentinelRuleRecord {
        rule_id: Uuid::new_v4().to_string(),
        trigger_type,
        trigger_config: trigger_config.to_string(),
        target_shortcut,
        is_active: true,
    };
    conn.execute(
        "INSERT INTO sentinel_rules (rule_id, trigger_type, trigger_config, target_shortcut, is_active)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            record.rule_id,
            serde_trigger_type(&record.trigger_type),
            record.trigger_config,
            record.target_shortcut,
            record.is_active,
        ],
    )
    .map_err(MoatError::Database)?;
    Ok(record)
}

pub fn list_sentinel_rules(conn: &Connection) -> MoatResult<Vec<SentinelRuleRecord>> {
    let mut stmt = conn
        .prepare(
            "SELECT rule_id, trigger_type, trigger_config, target_shortcut, is_active
             FROM sentinel_rules
             ORDER BY rule_id ASC",
        )
        .map_err(MoatError::Database)?;
    let rows = stmt
        .query_map([], |row| {
            let trigger_type: String = row.get(1)?;
            Ok(SentinelRuleRecord {
                rule_id: row.get(0)?,
                trigger_type: parse_trigger_type(&trigger_type).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
                    )
                })?,
                trigger_config: row.get(2)?,
                target_shortcut: row.get(3)?,
                is_active: row.get(4)?,
            })
        })
        .map_err(MoatError::Database)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MoatError::Database)
}

pub fn toggle_sentinel_rule(conn: &Connection, rule_id: &str, is_active: bool) -> MoatResult<()> {
    conn.execute(
        "UPDATE sentinel_rules SET is_active = ?2 WHERE rule_id = ?1",
        params![rule_id.trim(), is_active],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

pub fn delete_sentinel_rule(conn: &Connection, rule_id: &str) -> MoatResult<()> {
    conn.execute(
        "DELETE FROM sentinel_rules WHERE rule_id = ?1",
        params![rule_id.trim()],
    )
    .map_err(MoatError::Database)?;
    Ok(())
}

async fn install_cron_rule(
    scheduler: &JobScheduler,
    rule: &SentinelRuleRecord,
    trigger_handler: TriggerHandler,
) -> MoatResult<()> {
    let cron = normalize_cron_expression(&rule.trigger_config)?;
    let target_shortcut = rule.target_shortcut.clone();
    let job = Job::new_async(&cron, move |_job_id, _scheduler| {
        let target_shortcut = target_shortcut.clone();
        let trigger_handler = Arc::clone(&trigger_handler);
        Box::pin(async move {
            trigger_handler(target_shortcut);
        })
    })
    .map_err(|err| {
        MoatError::Settings(format!(
            "invalid sentinel cron '{}': {err}",
            rule.trigger_config
        ))
    })?;
    scheduler.add(job).await.map_err(|err| {
        MoatError::Orchestrator(format!("failed to add sentinel cron job: {err}"))
    })?;
    Ok(())
}

fn install_file_watch_rule(
    rule: &SentinelRuleRecord,
    trigger_handler: TriggerHandler,
    fault_handler: FaultHandler,
) -> MoatResult<RecommendedWatcher> {
    let matcher = compile_glob(&rule.trigger_config)?;
    let watch_root = watch_root_for_pattern(&rule.trigger_config)?;
    let target_shortcut = rule.target_shortcut.clone();
    let dedupe = Arc::new(StdMutex::new(HashMap::<PathBuf, Instant>::new()));
    let matcher = Arc::new(matcher);
    let pattern = rule.trigger_config.clone();

    let mut watcher = RecommendedWatcher::new(
        move |result: Result<Event, notify::Error>| match result {
            Ok(event) => {
                if !is_forward_file_event(&event.kind) {
                    return;
                }
                for path in &event.paths {
                    if !matcher.is_match(path) {
                        continue;
                    }
                    if !should_fire_path(&dedupe, path) {
                        continue;
                    }
                    trigger_handler(target_shortcut.clone());
                    break;
                }
            }
            Err(err) => fault_handler(SentinelFault {
                severity: "warning".into(),
                subsystem: "sentinel".into(),
                message: format!("file watch '{}' emitted error: {err}", pattern),
                code: Some("SENTINEL_FILEWATCH_EVENT_ERROR".into()),
            }),
        },
        Config::default(),
    )
    .map_err(|err| MoatError::Orchestrator(format!("failed to create sentinel watcher: {err}")))?;

    watcher
        .watch(&watch_root, RecursiveMode::Recursive)
        .map_err(|err| {
            MoatError::Orchestrator(format!("failed to watch '{}': {err}", watch_root.display()))
        })?;
    Ok(watcher)
}

fn parse_trigger_type(raw: &str) -> Result<SentinelTriggerType, String> {
    match raw.trim() {
        "CRON" => Ok(SentinelTriggerType::Cron),
        "FILE_WATCH" => Ok(SentinelTriggerType::FileWatch),
        other => Err(format!("unsupported sentinel trigger_type '{other}'")),
    }
}

fn serde_trigger_type(trigger_type: &SentinelTriggerType) -> &'static str {
    match trigger_type {
        SentinelTriggerType::Cron => "CRON",
        SentinelTriggerType::FileWatch => "FILE_WATCH",
    }
}

fn normalize_cron_expression(raw: &str) -> MoatResult<String> {
    let trimmed = raw.trim();
    let fields = trimmed.split_whitespace().count();
    match fields {
        5 => Ok(format!("0 {trimmed}")),
        6 | 7 => Ok(trimmed.to_string()),
        _ => Err(MoatError::Settings(format!(
            "sentinel CRON '{}' must have 5, 6, or 7 fields",
            raw
        ))),
    }
}

fn compile_glob(pattern: &str) -> MoatResult<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(pattern.trim()).map_err(|err| {
        MoatError::Settings(format!("invalid FILE_WATCH glob '{}': {err}", pattern))
    })?);
    builder
        .build()
        .map_err(|err| MoatError::Settings(format!("invalid FILE_WATCH glob '{}': {err}", pattern)))
}

fn watch_root_for_pattern(pattern: &str) -> MoatResult<PathBuf> {
    let trimmed = pattern.trim();
    let path = Path::new(trimmed);
    let mut root = PathBuf::new();
    let mut wildcard_seen = false;

    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if has_glob_magic(&text) {
            wildcard_seen = true;
            break;
        }
        root.push(component.as_os_str());
    }

    if root.as_os_str().is_empty() {
        return Err(MoatError::Settings(format!(
            "FILE_WATCH glob '{}' does not have a concrete watch root",
            pattern
        )));
    }

    if wildcard_seen {
        return Ok(root);
    }

    Ok(root.parent().map(Path::to_path_buf).unwrap_or(root))
}

fn has_glob_magic(component: &str) -> bool {
    component.contains('*') || component.contains('?') || component.contains('[')
}

fn is_forward_file_event(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
}

fn should_fire_path(cache: &Arc<StdMutex<HashMap<PathBuf, Instant>>>, path: &Path) -> bool {
    let now = Instant::now();
    let mut cache = cache.lock().expect("sentinel dedupe cache lock");
    cache.retain(|_, seen_at| {
        now.duration_since(*seen_at) <= Duration::from_millis(FILE_EVENT_DEDUPE_MS)
    });
    match cache.get(path) {
        Some(seen_at)
            if now.duration_since(*seen_at) <= Duration::from_millis(FILE_EVENT_DEDUPE_MS) =>
        {
            false
        }
        _ => {
            cache.insert(path.to_path_buf(), now);
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_cron_expression, parse_trigger_type, SentinelTriggerType};

    #[test]
    fn cron_5_field_is_upgraded() {
        assert_eq!(
            normalize_cron_expression("0 17 * * 5").unwrap(),
            "0 0 17 * * 5"
        );
    }

    #[test]
    fn parse_trigger_type_reads_file_watch() {
        assert_eq!(
            parse_trigger_type("FILE_WATCH").unwrap(),
            SentinelTriggerType::FileWatch
        );
    }
}

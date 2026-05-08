use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const EXECUTION_CONTEXT_BUDGET: usize = 2000;

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    pub key: String,
    pub expected_path: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub from_task: String,
    pub artifact_key: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ExecutionTask {
    pub task_id: String,
    pub assigned_worker: String,
    pub instructions: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub produces: Vec<ArtifactDescriptor>,
    #[serde(default)]
    pub consumes: Vec<ArtifactRef>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    #[serde(default = "uuid::Uuid::new_v4")]
    pub workflow_id: Uuid,
    #[serde(default)]
    pub session_id: Uuid,
    pub global_context: String,
    pub global_context_token_estimate: usize,
    pub tasks: Vec<ExecutionTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerDescriptor {
    pub id: String,
    pub description: String,
    pub effective_capabilities: Vec<String>,
}

#[derive(Debug)]
pub enum ExecutionValidationError {
    DuplicateTask(String),
    UnknownDependency {
        task_id: String,
        dependency: String,
    },
    UnknownProducer {
        task_id: String,
        producer: String,
    },
    UnknownArtifact {
        task_id: String,
        producer: String,
        artifact_key: String,
    },
    UnknownWorker(String),
    ContextTooLarge(usize),
    Cycle {
        a: String,
        b: String,
        message: String,
    },
}

impl std::fmt::Display for ExecutionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateTask(task_id) => write!(f, "duplicate task_id '{}'", task_id),
            Self::UnknownDependency {
                task_id,
                dependency,
            } => {
                write!(
                    f,
                    "task '{}' depends on unknown task '{}'",
                    task_id, dependency
                )
            }
            Self::UnknownProducer { task_id, producer } => {
                write!(
                    f,
                    "task '{}' consumes unknown producer '{}'",
                    task_id, producer
                )
            }
            Self::UnknownArtifact {
                task_id,
                producer,
                artifact_key,
            } => write!(
                f,
                "task '{}' consumes missing artifact '{}' from '{}'",
                task_id, artifact_key, producer
            ),
            Self::UnknownWorker(worker) => {
                write!(f, "assigned_worker '{}' is not registered", worker)
            }
            Self::ContextTooLarge(size) => write!(
                f,
                "global_context_token_estimate {} exceeds budget {}",
                size, EXECUTION_CONTEXT_BUDGET
            ),
            Self::Cycle { message, .. } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ExecutionValidationError {}

pub fn validate_execution_plan(
    plan: &ExecutionPlan,
    workers: &[WorkerDescriptor],
) -> Result<(), ExecutionValidationError> {
    if plan.global_context_token_estimate > EXECUTION_CONTEXT_BUDGET {
        return Err(ExecutionValidationError::ContextTooLarge(
            plan.global_context_token_estimate,
        ));
    }

    let worker_ids: HashSet<&str> = workers.iter().map(|worker| worker.id.as_str()).collect();
    let mut seen = HashSet::new();
    let mut produces = HashMap::<&str, HashSet<&str>>::new();
    for task in &plan.tasks {
        if !seen.insert(task.task_id.as_str()) {
            return Err(ExecutionValidationError::DuplicateTask(
                task.task_id.clone(),
            ));
        }
        if !worker_ids.contains(task.assigned_worker.as_str()) {
            return Err(ExecutionValidationError::UnknownWorker(
                task.assigned_worker.clone(),
            ));
        }
        produces.insert(
            task.task_id.as_str(),
            task.produces
                .iter()
                .map(|artifact| artifact.key.as_str())
                .collect(),
        );
    }

    for task in &plan.tasks {
        for dependency in &task.depends_on {
            if !seen.contains(dependency.as_str()) {
                return Err(ExecutionValidationError::UnknownDependency {
                    task_id: task.task_id.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
        for artifact in &task.consumes {
            if !seen.contains(artifact.from_task.as_str()) {
                return Err(ExecutionValidationError::UnknownProducer {
                    task_id: task.task_id.clone(),
                    producer: artifact.from_task.clone(),
                });
            }
            let Some(keys) = produces.get(artifact.from_task.as_str()) else {
                return Err(ExecutionValidationError::UnknownProducer {
                    task_id: task.task_id.clone(),
                    producer: artifact.from_task.clone(),
                });
            };
            if !keys.contains(artifact.artifact_key.as_str()) {
                return Err(ExecutionValidationError::UnknownArtifact {
                    task_id: task.task_id.clone(),
                    producer: artifact.from_task.clone(),
                    artifact_key: artifact.artifact_key.clone(),
                });
            }
        }
    }

    detect_execution_cycle(&plan.tasks)?;
    Ok(())
}

pub fn detect_execution_cycle(tasks: &[ExecutionTask]) -> Result<(), ExecutionValidationError> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        Visiting,
        Done,
    }

    fn visit<'a>(
        task_id: &'a str,
        tasks: &'a HashMap<&'a str, &'a ExecutionTask>,
        marks: &mut HashMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Result<(), ExecutionValidationError> {
        if marks.get(task_id) == Some(&Mark::Done) {
            return Ok(());
        }
        if marks.get(task_id) == Some(&Mark::Visiting) {
            let a = stack.last().copied().unwrap_or(task_id).to_string();
            return Err(ExecutionValidationError::Cycle {
                a,
                b: task_id.to_string(),
                message: format!("dependency cycle detected around '{}'", task_id),
            });
        }

        marks.insert(task_id, Mark::Visiting);
        stack.push(task_id);
        if let Some(task) = tasks.get(task_id) {
            for dependency in &task.depends_on {
                visit(dependency.as_str(), tasks, marks, stack)?;
            }
        }
        stack.pop();
        marks.insert(task_id, Mark::Done);
        Ok(())
    }

    let task_map: HashMap<&str, &ExecutionTask> = tasks
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect();
    let mut marks = HashMap::new();
    let mut stack = Vec::new();
    for task_id in task_map.keys().copied() {
        visit(task_id, &task_map, &mut marks, &mut stack)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        detect_execution_cycle, validate_execution_plan, ArtifactDescriptor, ArtifactRef,
        ExecutionPlan, ExecutionTask, WorkerDescriptor,
    };
    use uuid::Uuid;

    fn worker() -> WorkerDescriptor {
        WorkerDescriptor {
            id: "coder".into(),
            description: "Code worker".into(),
            effective_capabilities: vec!["fs:write".into()],
        }
    }

    #[test]
    fn detect_cycle_fails() {
        let tasks = vec![
            ExecutionTask {
                task_id: "a".into(),
                assigned_worker: "coder".into(),
                instructions: "one".into(),
                depends_on: vec!["b".into()],
                produces: vec![],
                consumes: vec![],
            },
            ExecutionTask {
                task_id: "b".into(),
                assigned_worker: "coder".into(),
                instructions: "two".into(),
                depends_on: vec!["a".into()],
                produces: vec![],
                consumes: vec![],
            },
        ];
        assert!(detect_execution_cycle(&tasks).is_err());
    }

    #[test]
    fn validate_plan_checks_artifacts() {
        let plan = ExecutionPlan {
            workflow_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            global_context: "ctx".into(),
            global_context_token_estimate: 12,
            tasks: vec![
                ExecutionTask {
                    task_id: "a".into(),
                    assigned_worker: "coder".into(),
                    instructions: "one".into(),
                    depends_on: vec![],
                    produces: vec![ArtifactDescriptor {
                        key: "out".into(),
                        expected_path: "out.txt".into(),
                    }],
                    consumes: vec![],
                },
                ExecutionTask {
                    task_id: "b".into(),
                    assigned_worker: "coder".into(),
                    instructions: "two".into(),
                    depends_on: vec!["a".into()],
                    produces: vec![],
                    consumes: vec![ArtifactRef {
                        from_task: "a".into(),
                        artifact_key: "out".into(),
                    }],
                },
            ],
        };
        assert!(validate_execution_plan(&plan, &[worker()]).is_ok());
    }
}

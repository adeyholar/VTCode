use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use self::session::resolve_scheduled_workspace_path;
use crate::notifications::{NotificationEvent, send_global_notification};
use vtcode_commons::VtCodePaths;

pub const MAX_SCHEDULED_TASKS: usize = 50;
pub const SESSION_TASK_EXPIRY_HOURS: i64 = 72;
pub const DISABLE_CRON_ENV: &str = "VTCODE_DISABLE_CRON";
pub const DURABLE_SCHEDULER_RUNTIME_HINT: &str = "Durable tasks fire while VT Code is open, `vtcode schedule serve` is running, or the installed scheduler service is active.";

const SERVICE_NAME: &str = "vtcode-scheduler";
const LAUNCHD_LABEL: &str = "com.vtcode.scheduler";

#[cfg(test)]
mod test_env_overrides {
    use std::sync::{LazyLock, Mutex};

    static DISABLE_CRON: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

    pub(super) fn get() -> Option<String> {
        DISABLE_CRON.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(super) fn set(value: Option<&str>) {
        if let Ok(mut slot) = DISABLE_CRON.lock() {
            *slot = value.map(ToString::to_string);
        }
    }
}

#[cfg(test)]
mod tests;

mod cron;
mod service;
mod session;
mod spec;
mod store;

use self::store::{release_claim_async, try_acquire_claim_async};

pub use self::service::{
    ServiceInstallPlan, ServiceManager, install_service_file, render_launchd_plist, render_service_install_plan,
    render_systemd_unit, uninstall_service_file,
};
#[cfg(test)]
pub(crate) use self::session::resolve_scheduled_workspace_path_with_home;
pub use self::session::{
    ScheduleCreateInput, SessionLanguageCommand, SessionScheduler, parse_local_datetime, parse_schedule_create_args,
    parse_schedule_create_tokens, parse_session_language_command,
};
pub use self::spec::{
    Cron5, DueSessionPrompt, FixedInterval, OneShot, ScheduleSpec, ScheduledTaskAction, ScheduledTaskDefinition,
    ScheduledTaskRecord, ScheduledTaskRuntimeState, ScheduledTaskSummary, TaskRunStatus,
};
pub use self::store::{DurableTaskStore, SchedulerPaths};

#[derive(Debug, Clone)]
pub struct SchedulerDaemon {
    store: DurableTaskStore,
    executable_path: PathBuf,
}

impl SchedulerDaemon {
    #[must_use]
    pub fn new(store: DurableTaskStore, executable_path: PathBuf) -> Self {
        Self { store, executable_path }
    }

    pub async fn serve_forever(&self) -> Result<()> {
        loop {
            self.run_due_tasks_once().await?;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn run_due_tasks_once(&self) -> Result<usize> {
        let now = Utc::now();
        let mut records = self.store.load_records_async().await?;
        let mut executed = 0usize;

        records.sort_by_key(|record| record.runtime.next_run_at);
        for mut record in records {
            let Some(next_run_at) = record.runtime.next_run_at else {
                continue;
            };
            if now < next_run_at {
                continue;
            }
            if !try_acquire_claim_async(self.store.paths().clone(), record.definition.id.clone()).await? {
                continue;
            }

            let result = self.execute_record(&record, now).await;
            let release_result = release_claim_async(self.store.paths().clone(), record.definition.id.clone()).await;
            let run_outcome = result?;
            release_result?;

            apply_run_outcome(&mut record, run_outcome)?;
            self.store.update_runtime_async(record).await?;
            executed = executed.saturating_add(1);
        }

        Ok(executed)
    }

    async fn execute_record(&self, record: &ScheduledTaskRecord, run_at: DateTime<Utc>) -> Result<RunOutcome> {
        match &record.definition.action {
            ScheduledTaskAction::Reminder { message } => {
                let notification = send_global_notification(NotificationEvent::IdlePrompt {
                    title: format!("Scheduled reminder: {}", record.definition.name),
                    message: message.clone(),
                })
                .await;
                let status = match notification {
                    Ok(()) => TaskRunStatus::ReminderSent,
                    Err(error) => TaskRunStatus::Failed {
                        message: format!("failed to send reminder notification: {error:#}"),
                    },
                };
                Ok(RunOutcome {
                    ran_at: run_at,
                    status,
                    artifact_dir: None,
                    events_file: None,
                    last_message_file: None,
                })
            }
            ScheduledTaskAction::Prompt { prompt } => {
                let artifact_dir = self.store.paths().artifact_dir(&record.definition.id, run_at);
                let events_file = artifact_dir.join("events.jsonl");
                let last_message_file = artifact_dir.join("last-message.txt");
                let workspace_label = scheduled_workspace_label(&record.definition);
                let workspace = resolve_scheduled_task_workspace(&record.definition);
                let execution = async {
                    VtCodePaths::ensure_user_dir(&artifact_dir)
                        .with_context(|| format!("Failed to create run artifact dir {}", artifact_dir.display()))?;

                    let mut command = Command::new(&self.executable_path);
                    command
                        .arg("exec")
                        .arg("--events")
                        .arg(&events_file)
                        .arg("--last-message-file")
                        .arg(&last_message_file)
                        .arg(prompt)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null());

                    if let Some(workspace) = workspace? {
                        command.current_dir(&workspace);
                    }

                    command.status().await.with_context(|| {
                        format!(
                            "Failed to spawn scheduled VT Code exec for task {} using {} in {}",
                            record.definition.id,
                            self.executable_path.display(),
                            workspace_label
                        )
                    })
                }
                .await;
                let run_status = match execution {
                    Ok(status) if status.success() => TaskRunStatus::Success,
                    Ok(status) => TaskRunStatus::Failed {
                        message: format!("vtcode exec exited with {status}"),
                    },
                    Err(error) => TaskRunStatus::Failed { message: format!("{error:#}") },
                };
                Ok(RunOutcome {
                    ran_at: run_at,
                    status: run_status,
                    artifact_dir: artifact_dir.is_dir().then_some(artifact_dir),
                    events_file: events_file.is_file().then_some(events_file),
                    last_message_file: last_message_file.is_file().then_some(last_message_file),
                })
            }
        }
    }
}

pub fn scheduled_tasks_enabled(enabled_in_config: bool) -> bool {
    #[cfg(test)]
    if let Some(value) = test_env_overrides::get() {
        let normalized = value.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
            return false;
        }
    }

    if let Ok(value) = std::env::var(DISABLE_CRON_ENV) {
        let normalized = value.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
            return false;
        }
    }
    enabled_in_config
}

#[must_use]
pub fn durable_task_is_overdue(
    next_run_at: Option<DateTime<Utc>>,
    last_run_at: Option<DateTime<Utc>>,
    has_last_status: bool,
    now: DateTime<Utc>,
) -> bool {
    !has_last_status && last_run_at.is_none() && next_run_at.is_some_and(|next_run| next_run <= now)
}

fn initialize_runtime_state(definition: &ScheduledTaskDefinition) -> Result<ScheduledTaskRuntimeState> {
    let next_base_run_at = definition.schedule.first_base_fire_at(definition.created_at)?;
    let next_run_at = next_base_run_at
        .map(|base| definition.schedule.jittered_fire_at(&definition.id, base))
        .transpose()?;
    Ok(ScheduledTaskRuntimeState {
        next_base_run_at,
        next_run_at,
        ..ScheduledTaskRuntimeState::default()
    })
}

#[derive(Debug, Clone, Copy)]
struct NextScheduledRun {
    base_fire_at: DateTime<Utc>,
    fire_at: DateTime<Utc>,
}

fn due_session_prompt(record: &ScheduledTaskRecord) -> Option<DueSessionPrompt> {
    let ScheduledTaskAction::Prompt { prompt } = &record.definition.action else {
        return None;
    };

    Some(DueSessionPrompt {
        id: record.definition.id.clone(),
        name: record.definition.name.clone(),
        prompt: prompt.clone(),
    })
}

fn next_scheduled_run(
    definition: &ScheduledTaskDefinition,
    last_base_run_at: Option<DateTime<Utc>>,
) -> Result<Option<NextScheduledRun>> {
    let Some(last_base_run_at) = last_base_run_at else {
        return Ok(None);
    };

    let Some(next_base_run_at) = definition.schedule.next_base_fire_after(last_base_run_at)? else {
        return Ok(None);
    };

    if definition.expires_at.is_some_and(|expiry| next_base_run_at > expiry) {
        return Ok(None);
    }

    Ok(Some(NextScheduledRun {
        fire_at: definition.schedule.jittered_fire_at(&definition.id, next_base_run_at)?,
        base_fire_at: next_base_run_at,
    }))
}

fn advance_record_runtime(
    record: &mut ScheduledTaskRecord,
    ran_at: DateTime<Utc>,
    status: TaskRunStatus,
) -> Result<bool> {
    record.runtime.last_run_at = Some(ran_at);
    record.runtime.last_status = Some(status);

    match next_scheduled_run(&record.definition, record.runtime.next_base_run_at)? {
        Some(next_run) => {
            record.runtime.next_base_run_at = Some(next_run.base_fire_at);
            record.runtime.next_run_at = Some(next_run.fire_at);
            Ok(false)
        }
        None => {
            record.runtime.next_base_run_at = None;
            record.runtime.next_run_at = None;
            Ok(true)
        }
    }
}

fn apply_run_outcome(record: &mut ScheduledTaskRecord, outcome: RunOutcome) -> Result<()> {
    let RunOutcome {
        ran_at,
        status,
        artifact_dir,
        events_file,
        last_message_file,
    } = outcome;

    record.runtime.last_artifact_dir = artifact_dir;
    record.runtime.last_events_file = events_file;
    record.runtime.last_message_file = last_message_file;

    advance_record_runtime(record, ran_at, status)?;

    Ok(())
}

#[derive(Debug, Clone)]
struct RunOutcome {
    ran_at: DateTime<Utc>,
    status: TaskRunStatus,
    artifact_dir: Option<PathBuf>,
    events_file: Option<PathBuf>,
    last_message_file: Option<PathBuf>,
}

fn scheduled_workspace_label(definition: &ScheduledTaskDefinition) -> String {
    definition
        .workspace
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<none>".to_string())
}

fn resolve_scheduled_task_workspace(definition: &ScheduledTaskDefinition) -> Result<Option<PathBuf>> {
    definition
        .workspace
        .as_deref()
        .map(resolve_scheduled_workspace_path)
        .transpose()
        .with_context(|| {
            format!(
                "Failed to resolve scheduled task workspace {} for task {}",
                scheduled_workspace_label(definition),
                definition.id
            )
        })
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    VtCodePaths::write_private_file_atomic(path, content)
        .with_context(|| format!("Failed to atomically write {}", path.display()))
}

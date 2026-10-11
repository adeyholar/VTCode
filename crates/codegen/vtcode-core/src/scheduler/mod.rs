use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use std::fs;
use std::io::Write;
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

const CLAIM_STALE_SECS: u64 = 15 * 60;
const SERVICE_NAME: &str = "vtcode-scheduler";
const LAUNCHD_LABEL: &str = "com.vtcode.scheduler";
const DURABLE_STORE_DIR: &str = "scheduled_tasks";

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

#[derive(Debug, Clone)]
pub struct SchedulerPaths {
    pub config_root: PathBuf,
    pub data_root: PathBuf,
}

impl SchedulerPaths {
    pub fn new_default() -> Result<Self> {
        let paths = VtCodePaths::resolve().context("Failed to resolve VT Code paths")?;
        let config_root = paths.config_path(DURABLE_STORE_DIR)?;
        let data_root = paths.state_path(DURABLE_STORE_DIR)?;
        Ok(Self { config_root, data_root })
    }

    #[must_use]
    pub fn tasks_dir(&self) -> PathBuf {
        self.config_root.join("tasks")
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.data_root.join("state")
    }

    #[must_use]
    pub fn claims_dir(&self) -> PathBuf {
        self.data_root.join("claims")
    }

    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.data_root.join("runs")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.config_root,
            &self.data_root,
            &self.tasks_dir(),
            &self.state_dir(),
            &self.claims_dir(),
            &self.runs_dir(),
        ] {
            VtCodePaths::ensure_user_dir(dir)
                .with_context(|| format!("Failed to create scheduler directory {}", dir.display()))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn definition_path(&self, id: &str) -> PathBuf {
        self.tasks_dir().join(format!("{id}.toml"))
    }

    #[must_use]
    pub fn runtime_path(&self, id: &str) -> PathBuf {
        self.state_dir().join(format!("{id}.json"))
    }

    #[must_use]
    pub fn claim_path(&self, id: &str) -> PathBuf {
        self.claims_dir().join(format!("{id}.claim"))
    }

    #[must_use]
    pub fn artifact_dir(&self, id: &str, run_at: DateTime<Utc>) -> PathBuf {
        self.runs_dir().join(id).join(run_at.format("%Y%m%dT%H%M%SZ").to_string())
    }
}

#[derive(Debug, Clone)]
pub struct DurableTaskStore {
    paths: SchedulerPaths,
}

impl DurableTaskStore {
    #[must_use]
    pub fn with_paths(paths: SchedulerPaths) -> Self {
        Self { paths }
    }

    pub fn new_default() -> Result<Self> {
        Ok(Self { paths: SchedulerPaths::new_default()? })
    }

    #[must_use]
    pub fn paths(&self) -> &SchedulerPaths {
        &self.paths
    }

    pub fn create(&self, definition: ScheduledTaskDefinition) -> Result<ScheduledTaskSummary> {
        self.paths.ensure_dirs()?;
        let current_count = self.definition_paths()?.len();
        if current_count >= MAX_SCHEDULED_TASKS {
            bail!("VT Code supports at most {MAX_SCHEDULED_TASKS} durable scheduled tasks");
        }

        let runtime = initialize_runtime_state(&definition)?;
        self.write_definition(&definition)?;
        self.write_runtime(&definition.id, &runtime)?;
        Ok(ScheduledTaskRecord { definition, runtime }.summary())
    }

    pub fn create_from_input(
        &self,
        input: ScheduleCreateInput,
        now: DateTime<Local>,
        default_workspace: Option<PathBuf>,
    ) -> Result<ScheduledTaskSummary> {
        let definition = input.build_definition(now, default_workspace)?;
        self.create(definition)
    }

    pub fn list(&self) -> Result<Vec<ScheduledTaskSummary>> {
        let mut records = self.load_records()?;
        records.sort_by_key(|record| record.runtime.next_run_at);
        Ok(records.into_iter().map(|record| record.summary()).collect())
    }

    pub fn delete(&self, id: &str) -> Result<Option<ScheduledTaskSummary>> {
        let Some(record) = self.load_record(id)? else {
            return Ok(None);
        };
        let _ = fs::remove_file(self.paths.definition_path(id));
        let _ = fs::remove_file(self.paths.runtime_path(id));
        let _ = fs::remove_file(self.paths.claim_path(id));
        Ok(Some(record.summary()))
    }

    pub fn load_record(&self, id: &str) -> Result<Option<ScheduledTaskRecord>> {
        let definition_path = self.paths.definition_path(id);
        if !definition_path.exists() {
            return Ok(None);
        }
        let definition = read_definition(&definition_path)?;
        let runtime = match self.read_runtime(&definition.id)? {
            Some(runtime) => runtime,
            None => initialize_runtime_state(&definition)?,
        };
        Ok(Some(ScheduledTaskRecord { definition, runtime }))
    }

    pub fn update_runtime(&self, record: &ScheduledTaskRecord) -> Result<()> {
        self.write_runtime(&record.definition.id, &record.runtime)
    }

    async fn load_records_async(&self) -> Result<Vec<ScheduledTaskRecord>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.load_records())
            .await
            .context("Scheduled task loading task panicked")?
    }

    async fn update_runtime_async(&self, record: ScheduledTaskRecord) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.update_runtime(&record))
            .await
            .context("Scheduled task runtime update task panicked")?
    }

    fn load_records(&self) -> Result<Vec<ScheduledTaskRecord>> {
        self.paths.ensure_dirs()?;
        let mut records = Vec::new();
        for definition_path in self.definition_paths()? {
            let definition = read_definition(&definition_path)?;
            let runtime = self
                .read_runtime(&definition.id)?
                .unwrap_or(initialize_runtime_state(&definition)?);
            records.push(ScheduledTaskRecord { definition, runtime });
        }
        Ok(records)
    }

    fn definition_paths(&self) -> Result<Vec<PathBuf>> {
        self.paths.ensure_dirs()?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(self.paths.tasks_dir())
            .with_context(|| format!("Failed to read {}", self.paths.tasks_dir().display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("toml") {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn write_definition(&self, definition: &ScheduledTaskDefinition) -> Result<()> {
        let serialized = toml::to_string_pretty(definition).context("Failed to serialize task definition")?;
        atomic_write(&self.paths.definition_path(&definition.id), serialized.as_bytes())
    }

    fn read_runtime(&self, id: &str) -> Result<Option<ScheduledTaskRuntimeState>> {
        let path = self.paths.runtime_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
        let runtime = serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(Some(runtime))
    }

    fn write_runtime(&self, id: &str, runtime: &ScheduledTaskRuntimeState) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(runtime).context("Failed to serialize runtime state")?;
        atomic_write(&self.paths.runtime_path(id), &serialized)
    }
}

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

fn read_definition(path: &Path) -> Result<ScheduledTaskDefinition> {
    let raw = fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    VtCodePaths::write_private_file_atomic(path, content)
        .with_context(|| format!("Failed to atomically write {}", path.display()))
}

fn try_acquire_claim(paths: &SchedulerPaths, id: &str) -> Result<bool> {
    paths.ensure_dirs()?;
    let path = paths.claim_path(id);
    match VtCodePaths::create_private_file(&path) {
        Ok(mut file) => {
            let timestamp = Utc::now().to_rfc3339();
            file.write_all(timestamp.as_bytes())
                .with_context(|| format!("Failed to write {}", path.display()))?;
            Ok(true)
        }
        Err(error)
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::AlreadyExists)
            }) =>
        {
            if claim_is_stale(&path)? {
                let _ = fs::remove_file(&path);
                return try_acquire_claim(paths, id);
            }
            Ok(false)
        }
        Err(error) => Err(error).with_context(|| format!("Failed to create {}", path.display())),
    }
}

async fn try_acquire_claim_async(paths: SchedulerPaths, id: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || try_acquire_claim(&paths, &id))
        .await
        .context("Scheduled task claim acquisition task panicked")?
}

fn claim_is_stale(path: &Path) -> Result<bool> {
    let metadata = fs::metadata(path).with_context(|| format!("Failed to stat {}", path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("Failed to read modification time for {}", path.display()))?;
    let elapsed = modified.elapsed().unwrap_or_default();
    Ok(elapsed >= Duration::from_secs(CLAIM_STALE_SECS))
}

fn release_claim(paths: &SchedulerPaths, id: &str) -> Result<()> {
    let path = paths.claim_path(id);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("Failed to remove {}", path.display()))?;
    }
    Ok(())
}

async fn release_claim_async(paths: SchedulerPaths, id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || release_claim(&paths, &id))
        .await
        .context("Scheduled task claim release task panicked")?
}

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use humantime::parse_duration as parse_human_duration;
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::process::Command;

use crate::notifications::{NotificationEvent, send_global_notification};
use crate::utils::path::normalize_path;
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
mod spec;

pub use self::service::{
    ServiceInstallPlan, ServiceManager, install_service_file, render_launchd_plist, render_service_install_plan,
    render_systemd_unit, uninstall_service_file,
};
pub use self::spec::{
    Cron5, DueSessionPrompt, FixedInterval, OneShot, ScheduleSpec, ScheduledTaskAction, ScheduledTaskDefinition,
    ScheduledTaskRecord, ScheduledTaskRuntimeState, ScheduledTaskSummary, TaskRunStatus,
};

static REMIND_AT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?ix)^\s*remind\s+me\s+at\s+(?P<when>.+?)\s+to\s+(?P<prompt>.+)\s*$").expect("remind-at regex")
});

static REMIND_IN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^\s*in\s+
        (?P<count>\d+)
        \s*
        (?P<unit>minutes|minute|hours|hour|days|day)
        \s*,?\s*
        (?P<prompt>.+)
        \s*$
    ",
    )
    .expect("remind-in regex")
});

static TIME_ONLY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?ix)^\s*(?P<hour>\d{1,2})(?::(?P<minute>\d{2}))?\s*(?P<ampm>am|pm)?\s*$").expect("time-only regex")
});

#[derive(Debug, Default, Clone)]
pub struct SessionScheduler {
    tasks: BTreeMap<String, ScheduledTaskRecord>,
}

impl SessionScheduler {
    #[must_use]
    pub fn new() -> Self {
        Self { tasks: BTreeMap::new() }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn create_prompt_task(
        &mut self,
        name: Option<String>,
        prompt: String,
        schedule: ScheduleSpec,
        created_at: DateTime<Utc>,
    ) -> Result<ScheduledTaskSummary> {
        self.ensure_capacity()?;
        let expires_at = schedule
            .is_recurring()
            .then(|| created_at + ChronoDuration::hours(SESSION_TASK_EXPIRY_HOURS));
        let definition = ScheduledTaskDefinition::new(
            name,
            schedule,
            ScheduledTaskAction::Prompt { prompt },
            None,
            created_at,
            expires_at,
        )?;
        let runtime = initialize_runtime_state(&definition)?;
        let record = ScheduledTaskRecord { definition: definition.clone(), runtime };
        let summary = record.summary();
        self.tasks.insert(definition.id.clone(), record);
        Ok(summary)
    }

    pub fn list(&self) -> Vec<ScheduledTaskSummary> {
        self.tasks.values().map(ScheduledTaskRecord::summary).collect()
    }

    pub fn delete(&mut self, query: &str) -> Option<ScheduledTaskSummary> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        if let Some(record) = self.tasks.remove(query) {
            return Some(record.summary());
        }
        let key = self
            .tasks
            .iter()
            .find_map(|(id, record)| record.definition.name.eq_ignore_ascii_case(query).then(|| id.clone()))?;
        self.tasks.remove(&key).map(|record| record.summary())
    }

    pub fn collect_due_prompts(&mut self, now: DateTime<Utc>) -> Result<Vec<DueSessionPrompt>> {
        let mut due = Vec::new();
        let mut completed = Vec::new();
        for record in self.tasks.values_mut() {
            let Some(next_run_at) = record.runtime.next_run_at else {
                continue;
            };
            if now < next_run_at {
                continue;
            }

            if let Some(prompt) = due_session_prompt(record) {
                due.push(prompt);
            }

            if advance_record_runtime(record, now, TaskRunStatus::Triggered)? {
                completed.push(record.definition.id.clone());
            }
        }

        for id in completed {
            self.tasks.remove(&id);
        }

        Ok(due)
    }

    fn ensure_capacity(&self) -> Result<()> {
        if self.tasks.len() >= MAX_SCHEDULED_TASKS {
            bail!("A session can hold at most {MAX_SCHEDULED_TASKS} scheduled tasks");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLanguageCommand {
    CreateOneShotPrompt { prompt: String, run_at: DateTime<Utc> },
    ListTasks,
    CancelTask { query: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleCreateInput {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub reminder: Option<String>,
    pub every: Option<String>,
    pub cron: Option<String>,
    pub at: Option<String>,
    pub workspace: Option<PathBuf>,
}

impl ScheduleCreateInput {
    pub fn build_definition(
        self,
        now: DateTime<Local>,
        default_workspace: Option<PathBuf>,
    ) -> Result<ScheduledTaskDefinition> {
        let action = match (self.prompt, self.reminder) {
            (Some(prompt), None) => ScheduledTaskAction::Prompt { prompt },
            (None, Some(reminder)) => ScheduledTaskAction::Reminder { message: reminder },
            (Some(_), Some(_)) => bail!("Choose either --prompt or --reminder"),
            (None, None) => bail!("One of --prompt or --reminder is required"),
        };

        let schedule = match (self.every, self.cron, self.at) {
            (Some(raw), None, None) => {
                let duration = parse_human_duration(raw.trim())
                    .with_context(|| format!("Invalid --every duration: {}", raw.trim()))?;
                ScheduleSpec::fixed_interval(duration)?
            }
            (None, Some(expression), None) => ScheduleSpec::cron5(expression)?,
            (None, None, Some(raw)) => ScheduleSpec::one_shot(parse_local_datetime(raw.trim(), now)?),
            _ => bail!("Choose exactly one of --every, --cron, or --at"),
        };

        let workspace = match (&action, self.workspace.or(default_workspace)) {
            (ScheduledTaskAction::Prompt { .. }, Some(path)) => Some(path),
            (ScheduledTaskAction::Prompt { .. }, None) => {
                bail!("Prompt tasks require a workspace (pass --workspace or create from chat)")
            }
            (ScheduledTaskAction::Reminder { .. }, path) => path,
        }
        .map(|path| resolve_scheduled_workspace_path(&path))
        .transpose()?;

        if matches!(action, ScheduledTaskAction::Prompt { .. }) && workspace.as_ref().is_some_and(|path| !path.is_dir())
        {
            let workspace = workspace.as_ref().context("prompt workspace should exist")?;
            bail!("Prompt task workspace does not exist or is not a directory: {}", workspace.display());
        }

        ScheduledTaskDefinition::new(self.name, schedule, action, workspace, now.with_timezone(&Utc), None)
    }
}

fn resolve_scheduled_workspace_path(path: &Path) -> Result<PathBuf> {
    resolve_scheduled_workspace_path_with_home(path, dirs::home_dir().as_deref())
}

fn resolve_scheduled_workspace_path_with_home(path: &Path, home_dir: Option<&Path>) -> Result<PathBuf> {
    let expanded = expand_scheduled_workspace_home(path, home_dir);
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .context("Failed to resolve current directory for scheduled task workspace")?
            .join(expanded)
    };
    Ok(normalize_path(&absolute))
}

fn expand_scheduled_workspace_home(path: &Path, home_dir: Option<&Path>) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };

    if raw == "~" {
        return home_dir.map(Path::to_path_buf).unwrap_or_else(|| path.to_path_buf());
    }

    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home_dir) = home_dir
    {
        return home_dir.join(rest);
    }

    path.to_path_buf()
}

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

pub fn parse_session_language_command(input: &str, now: DateTime<Local>) -> Option<Result<SessionLanguageCommand>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.trim_end_matches(['?', '.', '!']).to_ascii_lowercase();
    if normalized == "what scheduled tasks do i have" {
        return Some(Ok(SessionLanguageCommand::ListTasks));
    }

    if let Some(captures) = REMIND_AT_RE.captures(trimmed) {
        let when = captures.name("when").map(|value| value.as_str()).unwrap_or_default();
        let prompt = captures
            .name("prompt")
            .map(|value| value.as_str().trim().to_string())
            .unwrap_or_default();
        return Some(
            parse_local_datetime(when, now)
                .map(|run_at| SessionLanguageCommand::CreateOneShotPrompt { prompt, run_at }),
        );
    }

    if let Some(captures) = REMIND_IN_RE.captures(trimmed) {
        let count = captures["count"].parse::<i64>().ok()?;
        let prompt = captures["prompt"].trim().to_string();
        let delta = match captures["unit"].to_ascii_lowercase().as_str() {
            "minute" | "minutes" => ChronoDuration::minutes(count),
            "hour" | "hours" => ChronoDuration::hours(count),
            "day" | "days" => ChronoDuration::days(count),
            _ => return None,
        };
        return Some(Ok(SessionLanguageCommand::CreateOneShotPrompt {
            prompt,
            run_at: (now + delta).with_timezone(&Utc),
        }));
    }

    if let Some(query) = trimmed.strip_prefix("cancel ") {
        return Some(Ok(SessionLanguageCommand::CancelTask { query: query.trim().to_string() }));
    }

    None
}

pub fn parse_schedule_create_args(args: &str) -> Result<ScheduleCreateInput> {
    let tokens = shell_words::split(args).with_context(|| format!("Failed to parse arguments: {args}"))?;
    parse_schedule_create_tokens(&tokens)
}

pub fn parse_schedule_create_tokens(tokens: &[String]) -> Result<ScheduleCreateInput> {
    let mut name = None;
    let mut prompt = None;
    let mut reminder = None;
    let mut every = None;
    let mut cron = None;
    let mut at = None;
    let mut workspace = None;
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        let (flag, inline_value) = if let Some((left, right)) = token.split_once('=') {
            (left, Some(right.to_string()))
        } else {
            (token.as_str(), None)
        };

        let take_value = |idx: &mut usize| -> Result<String> {
            if let Some(value) = inline_value.clone() {
                return Ok(value);
            }
            let Some(value) = tokens.get(*idx + 1) else {
                bail!("Missing value for {flag}");
            };
            *idx += 1;
            Ok(value.clone())
        };

        match flag {
            "--name" => name = Some(take_value(&mut index)?),
            "--prompt" => prompt = Some(take_value(&mut index)?),
            "--reminder" => reminder = Some(take_value(&mut index)?),
            "--every" => every = Some(take_value(&mut index)?),
            "--cron" => cron = Some(take_value(&mut index)?),
            "--at" => at = Some(take_value(&mut index)?),
            "--workspace" => workspace = Some(PathBuf::from(take_value(&mut index)?)),
            "--help" | "help" => {
                bail!(
                    "Usage: /schedule create --prompt <text>|--reminder <text> --every <dur>|--cron <expr>|--at <time> [--name <label>] [--workspace <path>]"
                );
            }
            _ => bail!("Unknown option: {token}"),
        }
        index += 1;
    }

    Ok(ScheduleCreateInput { name, prompt, reminder, every, cron, at, workspace })
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

pub fn parse_local_datetime(raw: &str, now: DateTime<Local>) -> Result<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Time value cannot be empty");
    }

    if let Ok(parsed) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(parsed.with_timezone(&Utc));
    }

    for format in ["%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, format) {
            return localize_naive_datetime(naive).map(|value| value.with_timezone(&Utc));
        }
    }

    if let Some(captures) = TIME_ONLY_RE.captures(trimmed) {
        let mut hour = captures["hour"].parse::<u32>()?;
        let minute = captures
            .name("minute")
            .map(|value| value.as_str().parse::<u32>())
            .transpose()?
            .unwrap_or(0);
        if minute >= 60 {
            bail!("Invalid minute in time value");
        }
        if let Some(ampm) = captures.name("ampm").map(|value| value.as_str().to_ascii_lowercase()) {
            if hour == 0 || hour > 12 {
                bail!("Invalid 12-hour clock value");
            }
            if ampm == "pm" && hour != 12 {
                hour += 12;
            }
            if ampm == "am" && hour == 12 {
                hour = 0;
            }
        } else if hour >= 24 {
            bail!("Invalid 24-hour clock value");
        }

        let time = NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(|| anyhow!("Invalid time value"))?;
        let today = now.date_naive();
        let naive = today.and_time(time);
        let mut localized = localize_naive_datetime(naive)?;
        if localized <= now {
            localized = localize_naive_datetime((today + ChronoDuration::days(1)).and_time(time))?;
        }
        return Ok(localized.with_timezone(&Utc));
    }

    bail!("Unsupported time format. Use RFC3339, YYYY-MM-DD HH:MM, or a local time like 3pm")
}

fn localize_naive_datetime(naive: NaiveDateTime) -> Result<DateTime<Local>> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(first, _) => Ok(first),
        LocalResult::None => bail!("Local time does not exist due to timezone transition"),
    }
}

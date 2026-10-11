use crate::config::constants::defaults;
use crate::core::agent::snapshots::SnapshotTurnDiagnostics;
use crate::llm::provider::MessageContent;
use crate::telemetry::perf::PerfSpan;
use crate::utils::session_transcript_norm::normalize_distinct_tools_for_summary;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vtcode_commons::VtCodePaths;
use vtcode_commons::fs::{read_private_json_file, write_private_json_file};

mod identifier;
mod message;
mod metadata;
mod retention;
mod search;
mod settings;
mod snapshot;
mod transcript;

pub use self::identifier::{generate_session_archive_identifier, reserve_session_archive_identifier};
pub(crate) use self::identifier::{
    generate_unique_archive_path, reserve_new_session_archive_path, session_archive_path_for_identifier,
};
pub(crate) use self::retention::apply_session_retention_best_effort;
#[cfg(test)]
pub(crate) use self::retention::{SessionRetentionLimits, apply_session_retention_with_limits};
pub(crate) use self::transcript::{clean_transcript_lines, progress_transcript_from_recent_messages};

pub use self::message::SessionMessage;
pub use self::metadata::{
    SessionArchiveMetadata, SessionContinuationExhaustionReason, SessionContinuationMetadata,
    SessionContinuationRecommendedAction, SessionForkMode,
};

pub use self::search::{SearchResult, search_sessions};
pub use self::settings::{
    SESSION_DIR_ENV, SESSION_MAX_AGE_DAYS_ENV, SESSION_MAX_FILES_ENV, SESSION_MAX_SIZE_MB_ENV,
    apply_session_history_config_from_vtcode, history_persistence_enabled,
};
#[cfg(test)]
pub(crate) use self::settings::{
    SessionHistorySettings, clear_sessions_dir_override_for_tests, clear_test_env_override,
    override_sessions_dir_for_tests, set_test_env_override_path,
};
use self::settings::{read_env_var_os, session_history_settings};
pub use self::snapshot::{
    SessionListing, SessionProgress, SessionProgressArgs, SessionProgressPersistenceStatus, SessionSnapshot,
    VerifiedSessionArchiveIdentifier, session_listing_matches_workspace, session_workspace_path,
};

const SESSION_FILE_PREFIX: &str = "session";
const SESSION_FILE_EXTENSION: &str = "json";
const DEFAULT_SESSION_MAX_FILES: usize = 100;
const DEFAULT_SESSION_MAX_AGE_DAYS: u64 = 14;
const DEFAULT_SESSION_MAX_SIZE_MB: u64 = 100;
const BYTES_PER_MB: u64 = 1024 * 1024;

async fn resolve_sessions_dir_for_archive_writes() -> Result<PathBuf> {
    let sessions_dir = resolve_sessions_dir().await?;
    apply_session_retention_best_effort(&sessions_dir);
    Ok(sessions_dir)
}

#[derive(Debug, Clone)]
pub struct SessionArchive {
    path: PathBuf,
    metadata: SessionArchiveMetadata,
    started_at: DateTime<Utc>,
    progress_throttle: Arc<Mutex<ProgressThrottle>>,
}

#[derive(Debug)]
struct ProgressThrottle {
    last_written: Instant,
    last_turn: usize,
}

impl ProgressThrottle {
    fn new() -> Self {
        let min_interval = Duration::from_millis(defaults::DEFAULT_SESSION_PROGRESS_MIN_INTERVAL_MS);
        let last_written = Instant::now().checked_sub(min_interval).unwrap_or_else(Instant::now);
        Self { last_written, last_turn: 0 }
    }
}

impl SessionArchive {
    fn from_path(path: PathBuf, metadata: SessionArchiveMetadata, started_at: DateTime<Utc>) -> Self {
        Self {
            path,
            metadata,
            started_at,
            progress_throttle: Arc::new(Mutex::new(ProgressThrottle::new())),
        }
    }

    fn build_snapshot(
        &self,
        total_messages: usize,
        distinct_tools: Vec<String>,
        transcript: Vec<String>,
        messages: Vec<SessionMessage>,
        progress: Option<SessionProgress>,
    ) -> SessionSnapshot {
        use crate::utils::error_log_collector::drain_error_logs;

        SessionSnapshot {
            metadata: self.metadata.clone(),
            started_at: self.started_at,
            ended_at: Utc::now(),
            total_messages,
            distinct_tools,
            transcript,
            messages,
            progress: progress.map(Box::new),
            error_logs: drain_error_logs(),
        }
    }

    /// Records the active primary agent (session "mode") so a later resume can
    /// restore it. Called once the active agent is selected, after the archive
    /// and thread have been started.
    pub fn set_primary_agent(&mut self, primary_agent: impl Into<String>) {
        self.metadata.primary_agent = Some(primary_agent.into());
    }

    pub fn metadata(&self) -> &SessionArchiveMetadata {
        &self.metadata
    }

    fn build_final_snapshot(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
    ) -> SessionSnapshot {
        self.build_snapshot(
            total_messages,
            normalize_distinct_tools_for_summary(&distinct_tools),
            clean_transcript_lines(&transcript),
            messages,
            turn_diagnostics.map(|turn_diagnostics| SessionProgress {
                turn_diagnostics: Some(turn_diagnostics),
                ..SessionProgress::default()
            }),
        )
    }

    fn build_progress_snapshot(&self, args: SessionProgressArgs) -> SessionSnapshot {
        let SessionProgressArgs {
            total_messages,
            distinct_tools,
            messages,
            recent_messages,
            turn_number,
            token_usage,
            max_context_tokens,
            loaded_skills,
            turn_diagnostics,
        } = args;

        let transcript = progress_transcript_from_recent_messages(&recent_messages);
        let distinct_tools = normalize_distinct_tools_for_summary(&distinct_tools);
        let tool_summaries = distinct_tools.clone();

        self.build_snapshot(
            total_messages,
            distinct_tools,
            transcript,
            messages,
            Some(SessionProgress {
                turn_number,
                recent_messages,
                tool_summaries,
                token_usage,
                max_context_tokens,
                loaded_skills: loaded_skills.unwrap_or_default(),
                turn_diagnostics,
            }),
        )
    }

    pub async fn new(metadata: SessionArchiveMetadata, custom_suffix: Option<String>) -> Result<Self> {
        let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
        let started_at = Utc::now();
        let path = generate_unique_archive_path(&sessions_dir, &metadata, started_at, custom_suffix.as_deref());

        Ok(Self::from_path(path, metadata, started_at))
    }

    /// Create a session archive using an explicitly reserved session identifier.
    pub async fn new_with_identifier(metadata: SessionArchiveMetadata, session_identifier: String) -> Result<Self> {
        let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
        let path = reserve_new_session_archive_path(&sessions_dir, &session_identifier)?;

        Ok(Self::from_path(path, metadata, Utc::now()))
    }

    /// Reopen an existing archive file so follow-up runs can overwrite the snapshot in place.
    pub fn resume_from_listing(listing: &SessionListing, metadata: SessionArchiveMetadata) -> Self {
        Self::from_path(listing.path.clone(), metadata, listing.snapshot.started_at)
    }

    pub fn finalize(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
    ) -> Result<PathBuf> {
        self.finalize_with_diagnostics(transcript, total_messages, distinct_tools, messages, None)
    }

    /// Finalize the archive while retaining lightweight diagnostics for the
    /// last turn. The final snapshot still omits the heavy progress history.
    pub fn finalize_with_diagnostics(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
    ) -> Result<PathBuf> {
        let snapshot =
            self.build_final_snapshot(transcript, total_messages, distinct_tools, messages, turn_diagnostics);

        let path = self.write_snapshot(snapshot)?;
        if let Some(parent) = path.parent() {
            apply_session_retention_best_effort(parent);
        }
        Ok(path)
    }

    pub fn persist_progress(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", "sync");

        let snapshot = self.build_progress_snapshot(args);

        self.write_snapshot(snapshot)
    }

    /// Persist progress asynchronously and report whether this call wrote a
    /// new durable snapshot.
    ///
    /// A throttled checkpoint still returns the archive path for diagnostics,
    /// but it must not be treated as durable by callers that acknowledge
    /// state only after history has been written.
    pub async fn persist_progress_async_with_status(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, false).await
    }

    async fn persist_progress_async_with_status_inner(
        &self,
        args: SessionProgressArgs,
        bypass_throttle: bool,
    ) -> Result<SessionProgressPersistenceStatus> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", if bypass_throttle { "async_forced" } else { "async" });

        if !history_persistence_enabled() {
            return Ok(SessionProgressPersistenceStatus::Disabled(self.path.clone()));
        }
        if !bypass_throttle && !self.should_persist_progress(args.turn_number)? {
            return Ok(SessionProgressPersistenceStatus::Throttled(self.path.clone()));
        }

        let snapshot = self.build_progress_snapshot(args);
        let path = self.write_snapshot_async(snapshot).await?;
        Ok(SessionProgressPersistenceStatus::Persisted(path))
    }

    /// Persist progress immediately, bypassing the normal progress throttle.
    ///
    /// This is intended for terminal handoff checkpoints where the latest
    /// conversation must be durable before an external artifact is written.
    pub async fn persist_progress_async_with_status_forced(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, true).await
    }

    pub async fn persist_progress_async(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        Ok(self.persist_progress_async_with_status(args).await?.path().to_path_buf())
    }

    fn write_snapshot(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file_sync(&self.path, &snapshot)?;
        Ok(self.path.clone())
    }

    async fn write_snapshot_async(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file(&self.path, &snapshot).await?;
        Ok(self.path.clone())
    }

    fn should_persist_progress(&self, turn_number: usize) -> Result<bool> {
        let min_interval = Duration::from_millis(defaults::DEFAULT_SESSION_PROGRESS_MIN_INTERVAL_MS);
        let min_turns = defaults::DEFAULT_SESSION_PROGRESS_MIN_TURN_DELTA;

        let mut throttle = self
            .progress_throttle
            .lock()
            .map_err(|err| anyhow::anyhow!("session progress throttle lock poisoned: {err}"))
            .context("Failed to evaluate session progress persistence throttle")?;
        if turn_number <= throttle.last_turn {
            return Ok(false);
        }
        if throttle.last_written.elapsed() < min_interval && turn_number.saturating_sub(throttle.last_turn) < min_turns
        {
            return Ok(false);
        }

        throttle.last_written = Instant::now();
        throttle.last_turn = turn_number;
        Ok(true)
    }
    /// Update loaded skills in the archive metadata
    pub fn set_loaded_skills(&mut self, skills: Vec<String>) {
        self.metadata.loaded_skills = skills;
    }

    /// Update continuation metadata in the archive metadata.
    pub fn set_continuation_metadata(&mut self, continuation_metadata: Option<SessionContinuationMetadata>) {
        self.metadata.continuation_metadata = continuation_metadata;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify that a persisted snapshot resolves back to this archive and
    /// return its typed resume identifier.
    pub async fn verify_persisted_resume_identifier(
        &self,
        persisted_path: &Path,
    ) -> Result<Option<VerifiedSessionArchiveIdentifier>> {
        if self.path != persisted_path {
            return Ok(None);
        }

        let Some(identifier) = persisted_path.file_stem().and_then(|stem| stem.to_str()) else {
            return Ok(None);
        };
        let Some(listing) = find_session_by_identifier(identifier).await? else {
            return Ok(None);
        };
        if listing.path != self.path {
            return Ok(None);
        }

        Ok(Some(listing.verified_identifier()?))
    }

    /// Create a forked session from an existing session snapshot
    ///
    /// This creates a new session archive that inherits metadata from the source
    /// session but operates independently. The forked session will have a new
    /// archive file with a custom suffix if provided.
    ///
    /// # Arguments
    /// * `source_snapshot` - The snapshot of the session to fork from
    /// * `custom_suffix` - Optional custom suffix for the new session ID
    ///
    /// # Returns
    /// A new SessionArchive instance for the forked session
    pub async fn fork(source_snapshot: &SessionSnapshot, custom_suffix: Option<String>) -> Result<Self> {
        create_fork_archive(source_snapshot, custom_suffix, None).await
    }
}

async fn create_fork_archive(
    source_snapshot: &SessionSnapshot,
    custom_suffix: Option<String>,
    explicit_identifier: Option<String>,
) -> Result<SessionArchive> {
    let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
    let started_at = Utc::now();

    let forked_metadata = source_snapshot.metadata.fork_seed();

    let path = if let Some(session_identifier) = explicit_identifier {
        reserve_new_session_archive_path(&sessions_dir, &session_identifier)?
    } else {
        generate_unique_archive_path(&sessions_dir, &forked_metadata, started_at, custom_suffix.as_deref())
    };

    Ok(SessionArchive::from_path(path, forked_metadata, started_at))
}

pub async fn list_recent_sessions(limit: usize) -> Result<Vec<SessionListing>> {
    let sessions_dir = match resolve_sessions_dir().await {
        Ok(dir) => dir,
        Err(_) => return Ok(Vec::new()),
    };

    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    // `std::fs::read_dir` is a blocking directory scan; run it off the async
    // executor and collect paths before spawning parallel read tasks. See `# Blocking`
    // docs in `src/agent/runloop/git.rs`.
    let session_paths: Vec<PathBuf> = tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&sessions_dir)
            .with_context(|| format!("failed to read session directory: {}", sessions_dir.display()))?
        {
            let entry = entry.with_context(|| format!("failed to read session entry in {}", sessions_dir.display()))?;
            let path = entry.path();
            if is_session_file(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    })
    .await
    .context("session directory scan task panicked")??;

    // Process session files in parallel for better performance with large archives
    // Batch processing to avoid overwhelming the system with too many concurrent tasks
    const BATCH_SIZE: usize = 10;
    let mut all_listings = Vec::new();

    for batch in session_paths.chunks(BATCH_SIZE) {
        let mut tasks = Vec::with_capacity(batch.len());

        for path in batch {
            let path = path.clone();
            let task = tokio::task::spawn(async move {
                read_private_json_file::<SessionSnapshot>(&path)
                    .await
                    .ok()
                    .map(|snapshot| SessionListing { path, snapshot })
            });
            tasks.push(task);
        }

        // Collect results from this batch
        for task in tasks {
            if let Ok(Some(listing)) = task.await {
                all_listings.push(listing);
            }
        }
    }

    // Sort and limit results
    all_listings.sort_by_key(|a| std::cmp::Reverse(a.snapshot.ended_at));
    if limit > 0 && all_listings.len() > limit {
        all_listings.truncate(limit);
    }

    Ok(all_listings)
}

/// Find a session archive by its identifier (file stem) without needing to list all sessions.
pub async fn find_session_by_identifier(identifier: &str) -> Result<Option<SessionListing>> {
    let sessions_dir = match resolve_sessions_dir().await {
        Ok(dir) => dir,
        Err(_) => return Ok(None),
    };

    if !sessions_dir.exists() {
        return Ok(None);
    }

    let path = match session_archive_path_for_identifier(&sessions_dir, identifier) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect session archive {}", path.display()));
        }
    }

    let snapshot: SessionSnapshot = read_private_json_file(&path).await?;
    Ok(Some(SessionListing { path, snapshot }))
}

fn write_private_json_file_sync<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let contents =
        serde_json::to_vec_pretty(value).with_context(|| format!("failed to serialize {}", path.display()))?;
    VtCodePaths::write_private_file_atomic(path, &contents)
        .with_context(|| format!("failed to write private session archive {}", path.display()))
}

/// Find a saved session by ID or name for TUI `/resume` command.
///
/// Searches both the session identifier (UUID or file stem) and matches
/// against recent sessions. Returns a descriptive error message when no
/// matching session is found.
pub async fn find_session_by_id_or_name(id_or_name: &str) -> Result<SessionListing> {
    // First try exact identifier match (UUID or file stem)
    if let Some(listing) = find_session_by_identifier(id_or_name).await? {
        return Ok(listing);
    }

    // Fall back to searching recent sessions by identifier substring
    let listings = list_recent_sessions(200).await?;
    let normalized_query = id_or_name.to_lowercase();

    for listing in &listings {
        // Match against the file stem (session identifier)
        if let Some(stem) = listing.path.file_stem()
            && let Some(stem_str) = stem.to_str()
            && stem_str.to_lowercase().contains(&normalized_query)
        {
            return Ok(listing.clone());
        }
    }

    Err(anyhow::anyhow!("No saved chat found matching '{id_or_name}'."))
}

async fn resolve_sessions_dir() -> Result<PathBuf> {
    if let Some(custom) = read_env_var_os(SESSION_DIR_ENV) {
        let path = PathBuf::from(custom);
        VtCodePaths::ensure_user_dir(&path)
            .with_context(|| format!("failed to initialize session directory {}", path.display()))?;
        return Ok(path);
    }

    let paths = VtCodePaths::resolve().context("failed to resolve VT Code paths")?;
    paths
        .ensure_state_dir()
        .context("failed to initialize VT Code state directory")?;
    let dir = paths.state_path("sessions")?;
    VtCodePaths::ensure_user_dir(&dir)
        .with_context(|| format!("failed to initialize session directory {}", dir.display()))?;
    Ok(dir)
}

fn truncate_preview(input: &str, max_chars: usize) -> String {
    vtcode_commons::formatting::truncate_within(input, max_chars, "…")
}

fn compact_snapshot_to_max_bytes(mut snapshot: SessionSnapshot, max_bytes: usize) -> Result<SessionSnapshot> {
    if max_bytes == 0 {
        minimize_snapshot_payload(&mut snapshot);
        return Ok(snapshot);
    }

    while serde_json::to_vec(&snapshot)?.len() > max_bytes {
        if trim_oldest_snapshot_entries(&mut snapshot) {
            continue;
        }
        if strip_snapshot_overhead(&mut snapshot) {
            continue;
        }
        if shrink_snapshot_strings(&mut snapshot) {
            continue;
        }
        break;
    }

    if serde_json::to_vec(&snapshot)?.len() > max_bytes {
        minimize_snapshot_payload(&mut snapshot);
        let _ = shrink_snapshot_strings(&mut snapshot);
    }

    Ok(snapshot)
}

fn prepare_snapshot_for_write(snapshot: SessionSnapshot) -> Result<Option<SessionSnapshot>> {
    if !history_persistence_enabled() {
        return Ok(None);
    }

    let max_bytes = session_history_settings().max_bytes;
    let snapshot = match max_bytes {
        Some(max_bytes) => compact_snapshot_to_max_bytes(snapshot, max_bytes)?,
        None => snapshot,
    };

    Ok(Some(snapshot))
}

fn minimize_snapshot_payload(snapshot: &mut SessionSnapshot) {
    snapshot.messages.clear();
    snapshot.transcript.clear();
    snapshot.distinct_tools.clear();
    snapshot.error_logs.clear();
    if let Some(progress) = snapshot.progress.as_mut() {
        progress.recent_messages.clear();
        progress.tool_summaries.clear();
        progress.token_usage = None;
        progress.max_context_tokens = None;
        progress.loaded_skills.clear();
    }
}

fn trim_oldest_snapshot_entries(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = false;

    if snapshot.messages.len() > 1 {
        snapshot.messages.remove(0);
        changed = true;
    }

    if snapshot.transcript.len() > 1 {
        snapshot.transcript.remove(0);
        changed = true;
    }

    if let Some(progress) = snapshot.progress.as_mut()
        && progress.recent_messages.len() > 1
    {
        progress.recent_messages.remove(0);
        changed = true;
    }

    changed
}

fn strip_snapshot_overhead(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = false;

    if !snapshot.transcript.is_empty() {
        snapshot.transcript.clear();
        changed = true;
    }
    if !snapshot.distinct_tools.is_empty() {
        snapshot.distinct_tools.clear();
        changed = true;
    }
    if !snapshot.error_logs.is_empty() {
        snapshot.error_logs.clear();
        changed = true;
    }

    if let Some(progress) = snapshot.progress.as_mut() {
        if !progress.tool_summaries.is_empty() {
            progress.tool_summaries.clear();
            changed = true;
        }
        if progress.token_usage.take().is_some() {
            changed = true;
        }
        if progress.max_context_tokens.take().is_some() {
            changed = true;
        }
        if !progress.loaded_skills.is_empty() {
            progress.loaded_skills.clear();
            changed = true;
        }
    }

    changed
}

fn shrink_snapshot_strings(snapshot: &mut SessionSnapshot) -> bool {
    let mut changed = shrink_snapshot_metadata(&mut snapshot.metadata);

    for transcript in &mut snapshot.transcript {
        changed |= shrink_string(transcript);
    }

    for message in &mut snapshot.messages {
        changed |= shrink_session_message(message);
    }

    for error_log in &mut snapshot.error_logs {
        changed |= shrink_string(&mut error_log.message);
    }

    if let Some(progress) = snapshot.progress.as_mut() {
        for message in &mut progress.recent_messages {
            changed |= shrink_session_message(message);
        }
        if let Some(token_usage) = progress.token_usage.as_mut() {
            changed |= shrink_string(token_usage);
        }
    }

    changed
}

fn shrink_session_message(message: &mut SessionMessage) -> bool {
    let mut changed = false;
    changed |= shrink_message_content(&mut message.content);

    if let Some(reasoning) = message.reasoning.as_mut() {
        changed |= shrink_string(reasoning);
    }
    if message.reasoning_details.take().is_some() {
        changed = true;
    }
    if let Some(tool_call_id) = message.tool_call_id.as_mut() {
        changed |= shrink_string(tool_call_id);
    }
    if let Some(origin_tool) = message.origin_tool.as_mut() {
        changed |= shrink_string(origin_tool);
    }
    if let Some(tool_calls) = message.tool_calls.as_mut() {
        for tool_call in tool_calls.iter_mut() {
            changed |= shrink_string(&mut tool_call.id);
            changed |= shrink_string(&mut tool_call.call_type);
            if let Some(function) = tool_call.function.as_mut() {
                changed |= shrink_string(&mut function.name);
                changed |= shrink_string(&mut function.arguments);
            }
            if let Some(text) = tool_call.text.as_mut() {
                changed |= shrink_string(text);
            }
            if let Some(thought_signature) = tool_call.thought_signature.as_mut() {
                changed |= shrink_string(thought_signature);
            }
        }
    }

    changed
}

fn shrink_snapshot_metadata(metadata: &mut SessionArchiveMetadata) -> bool {
    let mut changed = false;

    changed |= shrink_string(&mut metadata.workspace_label);
    changed |= shrink_string(&mut metadata.workspace_path);
    changed |= shrink_string(&mut metadata.model);
    changed |= shrink_string(&mut metadata.provider);
    changed |= shrink_string(&mut metadata.theme);
    changed |= shrink_string(&mut metadata.reasoning_effort);
    changed |= shrink_optional_string(&mut metadata.debug_log_path);
    changed |= shrink_optional_string(&mut metadata.prompt_cache_lineage_id);
    changed |= shrink_optional_string(&mut metadata.external_thread_id);
    changed |= shrink_optional_string(&mut metadata.parent_session_id);
    changed |= shrink_optional_string(&mut metadata.primary_agent);

    for skill in &mut metadata.loaded_skills {
        changed |= shrink_string(skill);
    }

    if let Some(continuation) = metadata.continuation_metadata.as_mut() {
        if continuation.max_budget_usd_micros == Some(0) {
            continuation.max_budget_usd_micros = None;
            changed = true;
        }
        if continuation.actual_cost_usd_micros == Some(0) {
            continuation.actual_cost_usd_micros = None;
            changed = true;
        }
        if !continuation.summary_available
            && continuation.recommended_action == Some(SessionContinuationRecommendedAction::ContinueFromSummary)
        {
            continuation.recommended_action = None;
            changed = true;
        }
    }

    changed
}

fn shrink_message_content(content: &mut MessageContent) -> bool {
    match content {
        MessageContent::Text(text) => shrink_string(text),
        MessageContent::Parts(parts) => {
            let mut changed = false;
            for part in parts {
                changed |= match part {
                    crate::llm::provider::ContentPart::Text { text } => shrink_string(text),
                    crate::llm::provider::ContentPart::Image { data, mime_type, .. } => {
                        shrink_string(data) | shrink_string(mime_type)
                    }
                    crate::llm::provider::ContentPart::File { filename, file_id, file_data, file_url, .. } => {
                        shrink_optional_string(filename)
                            | shrink_optional_string(file_id)
                            | shrink_optional_string(file_data)
                            | shrink_optional_string(file_url)
                    }
                };
            }
            changed
        }
    }
}

fn shrink_optional_string(value: &mut Option<String>) -> bool {
    value.as_mut().is_some_and(shrink_string)
}

fn shrink_string(value: &mut String) -> bool {
    const MIN_RETAINED_CHARS: usize = 8;
    const TRUNCATION_MARKER: &str = "...";

    if value.len() <= MIN_RETAINED_CHARS + TRUNCATION_MARKER.len() {
        return false;
    }

    let keep_len = (value.len() / 2).max(MIN_RETAINED_CHARS);
    let prefix_len = keep_len.saturating_sub(TRUNCATION_MARKER.len());
    value.truncate(prefix_len);
    value.push_str(TRUNCATION_MARKER);
    true
}

fn is_session_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(ext)
            if ext.eq_ignore_ascii_case(SESSION_FILE_EXTENSION)
                || ext.eq_ignore_ascii_case("jsonl")
                || ext.eq_ignore_ascii_case("log")
    )
}

#[cfg(test)]
mod metadata_compat_tests {
    use super::{SessionArchive, SessionArchiveMetadata};

    #[test]
    fn primary_agent_metadata_is_backward_compatible() {
        let metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
            "workspace_label": "ws",
            "workspace_path": "/tmp/ws",
            "model": "gpt-5.6-sol",
            "provider": "openai",
            "theme": "test",
            "reasoning_effort": "low"
        }))
        .expect("deserialize metadata without primary_agent");

        assert_eq!(metadata.primary_agent, None);

        let value = serde_json::to_value(&metadata).expect("serialize metadata");
        assert!(value.get("primary_agent").is_none());
    }

    #[test]
    fn set_primary_agent_records_mode_on_archive() {
        let metadata = SessionArchiveMetadata::new("ws", "/tmp/ws", "gpt-5.6-sol", "openai", "test", "low");
        let mut archive = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(SessionArchive::new_with_identifier(metadata, "ws-0000".to_string()))
            .expect("create archive");
        assert_eq!(archive.metadata().primary_agent, None);

        archive.set_primary_agent("plan");
        assert_eq!(archive.metadata().primary_agent.as_deref(), Some("plan"));
    }
}

#[cfg(test)]
#[path = "session_archive_tests.rs"]
mod tests;

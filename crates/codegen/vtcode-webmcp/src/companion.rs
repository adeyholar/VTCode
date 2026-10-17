//! Transport-neutral task interface for the opt-in VT Code Companion.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Requested execution policy. Planning is always the default.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    /// Run through the read-only exec policy.
    #[default]
    Plan,
    /// Implement after a successful planning task and explicit user request.
    Implement,
}

/// Arguments accepted from the MCP client; workspace and executable are server-owned.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartTaskInput {
    /// Coding task, treated as prompt data rather than command-line syntax.
    pub prompt: String,
    /// Defaults to read-only planning.
    #[serde(default)]
    pub mode: TaskMode,
    /// Required for implementation; identifies a completed plan from this service.
    pub plan_task_id: Option<String>,
}

/// Identify a task owned by this service instance.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// Opaque identity returned by start_task.
    pub task_id: String,
}

/// Process lifecycle, independent of verification.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Execution resources are still owned by the service.
    Running,
    /// Both the process and canonical thread completed successfully.
    Completed,
    /// Execution or event collection failed.
    Failed,
    /// Cancellation completed and the child was reaped.
    Cancelled,
}

/// Verification is never inferred from the agent's final message or process exit.
#[derive(Debug, Clone, Copy, Default, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskVerification {
    /// No current-generation terminal verification was observed.
    #[default]
    Unverified,
    /// The runtime recorded a successful verifier with exit zero.
    Verified,
    /// The runtime recorded a failed verifier.
    Failed,
}

/// Bounded public projection of canonical task evidence.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TaskSnapshot {
    /// Service-assigned identity, distinct from the runtime session identity.
    pub task_id: String,
    /// Requested execution policy.
    pub mode: TaskMode,
    /// Current process lifecycle.
    pub state: TaskState,
    /// Canonical execution session, when observed.
    pub session_id: Option<String>,
    /// Last public agent response, sanitized and bounded.
    pub final_response: Option<String>,
    /// Successful recorded changes only; never a workspace-wide attribution claim.
    pub changes: Vec<String>,
    /// Recorded unified diff, sanitized and bounded.
    pub unified_diff: String,
    /// Whether public evidence was omitted, withheld, or collection was incomplete.
    pub evidence_truncated: bool,
    /// Current verification outcome.
    pub verification: TaskVerification,
    /// Most recent recorded verifier command, when available.
    pub verification_command: Option<String>,
    /// Exit code from the recorded verifier, never the task process exit.
    pub verification_exit_code: Option<i32>,
    /// Bounded public lifecycle description. Raw stderr and private reasoning are excluded.
    pub progress: String,
}

impl TaskSnapshot {
    /// Invalidate proof after later tool activity; old commands cannot describe
    /// the current mutation generation.
    pub fn invalidate_verification(&mut self) {
        self.verification = TaskVerification::Unverified;
        self.verification_command = None;
        self.verification_exit_code = None;
    }

    /// A stopped or failed collector may have missed later evidence. Retain a
    /// known verifier failure, but never retain a positive completion claim.
    pub fn mark_incomplete(&mut self) {
        self.evidence_truncated = true;
        if self.verification == TaskVerification::Verified {
            self.invalidate_verification();
        }
    }
}

/// Binary-owned execution boundary. The transport never launches processes.
#[async_trait]
pub trait CompanionTasks: Send + Sync {
    /// Start one task or reject it without side effects.
    async fn start(&self, input: StartTaskInput) -> Result<TaskSnapshot, String>;
    /// Inspect a task owned by this service.
    async fn get(&self, task_id: &str) -> Result<TaskSnapshot, String>;
    /// Request cancellation. False means the task is already terminal.
    async fn cancel(&self, task_id: &str) -> Result<bool, String>;
}

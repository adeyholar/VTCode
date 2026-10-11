//! Request-scoped validation recovery; independent of tracker progress budgets.

use serde_json::Value;
use vtcode_core::config::constants::tools;
use vtcode_core::types::CompactStr;

pub(crate) const PREFLIGHT_CORRECTIVE_FOLLOW_UP_PREFIX: &str = "Preflight corrective turn:";

#[derive(Debug, Default, PartialEq, Eq)]
enum RecoveryPhase {
    #[default]
    Collecting,
    Synthesis,
    Corrective,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidationCategory {
    Arguments,
    MalformedJson,
}

#[derive(Debug, PartialEq)]
struct RejectedCall {
    tool_name: CompactStr,
    category: ValidationCategory,
    arguments: Option<Value>,
}

#[derive(Debug, Default)]
pub(crate) struct PreflightRecovery {
    phase: RecoveryPhase,
    rejected_calls: Vec<RejectedCall>,
}

fn operational_arguments(arguments: Option<&Value>) -> Option<Value> {
    arguments.map(|arguments| {
        let mut arguments = arguments.clone();
        if let Some(object) = arguments.as_object_mut() {
            object.remove("max_output_tokens");
        }
        arguments
    })
}

#[derive(PartialEq, Eq)]
enum TrackerTarget {
    Item(Vec<u64>),
    Checklist,
}

fn index_path(arguments: &Value, key: &str) -> Option<Vec<u64>> {
    arguments
        .get(key)?
        .as_str()?
        .trim()
        .split('.')
        .map(|part| part.parse::<u64>().ok().filter(|index| *index > 0))
        .collect()
}

fn tracker_targets(arguments: &Value, planning_active: bool) -> Vec<TrackerTarget> {
    let mut targets = Vec::with_capacity(3);
    let index = arguments
        .get("index")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
    if let Some(index) = index.filter(|index| *index > 0 || !planning_active) {
        targets.push(TrackerTarget::Item(vec![index]));
    }
    if let Some(path) = index_path(arguments, "index_path") {
        targets.push(TrackerTarget::Item(path));
    }
    if arguments
        .get("items")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
    {
        targets.push(TrackerTarget::Checklist);
    }
    targets
}

fn valid_text_fields_match(previous: &Value, current: &Value, keys: &[&str]) -> bool {
    keys.iter().all(|key| {
        previous
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_none_or(|value| current.get(key).and_then(Value::as_str) == Some(value))
    })
}

fn tracker_operation_matches(previous: &Value, current: &Value, planning_active: bool) -> bool {
    // An invalid/missing action is repairable, not an operation identifier.
    let Some(action @ ("create" | "update" | "list" | "add")) = previous.get("action").and_then(Value::as_str) else {
        return true;
    };
    if current.get("action").and_then(Value::as_str) != Some(action) {
        return false;
    }
    match action {
        "update" => {
            let previous_targets = tracker_targets(previous, planning_active);
            let current_targets = tracker_targets(current, planning_active);
            previous_targets.is_empty() || previous_targets.iter().any(|target| current_targets.contains(target))
        }
        "create" => valid_text_fields_match(previous, current, &["title"]),
        "add" => {
            valid_text_fields_match(previous, current, &["description"])
                && index_path(previous, "parent_index_path")
                    .is_none_or(|path| index_path(current, "parent_index_path") == Some(path))
        }
        _ => true,
    }
}

fn operation_matches(tool_name: &str, previous: &Value, current: &Value, planning_active: bool) -> bool {
    match tool_name {
        tools::TASK_TRACKER => tracker_operation_matches(previous, current, planning_active),
        tools::EXEC_COMMAND | tools::SHELL => valid_text_fields_match(previous, current, &["cmd", "command"]),
        tools::CODE_SEARCH => valid_text_fields_match(previous, current, &["query", "path"]),
        tools::READ_FILE | tools::WRITE_FILE => valid_text_fields_match(previous, current, &["path", "file_path"]),
        // Preserve common selectors for tools without a dedicated operation key.
        _ => valid_text_fields_match(previous, current, &["action", "cmd", "command", "path", "file_path"]),
    }
}

impl PreflightRecovery {
    /// Returns whether this is a repeated identity, independent of JSON key order.
    pub(crate) fn reject(
        &mut self,
        tool_name: &str,
        category: ValidationCategory,
        arguments: Option<&Value>,
        cap: usize,
    ) -> bool {
        let identity = RejectedCall {
            tool_name: tool_name.into(),
            category,
            arguments: operational_arguments(arguments),
        };
        let repeated = self.rejected_calls.contains(&identity);
        if repeated && self.phase == RecoveryPhase::Corrective {
            self.phase = RecoveryPhase::Exhausted;
        }
        if !repeated && self.rejected_calls.len() < cap {
            self.rejected_calls.push(identity);
        }
        repeated
    }

    pub(crate) fn synthesis(&mut self) {
        self.phase = match self.phase {
            RecoveryPhase::Collecting | RecoveryPhase::Synthesis => RecoveryPhase::Synthesis,
            RecoveryPhase::Corrective | RecoveryPhase::Exhausted => RecoveryPhase::Exhausted,
        };
    }

    pub(crate) fn is_clear(&self) -> bool {
        self.rejected_calls.is_empty()
    }

    pub(crate) fn corrective_pending(&self) -> bool {
        self.phase == RecoveryPhase::Synthesis
    }

    pub(crate) fn begin_corrective_turn(&mut self) {
        if self.phase == RecoveryPhase::Synthesis {
            self.phase = RecoveryPhase::Corrective;
        }
    }

    pub(crate) fn finish_corrective_turn(&mut self) {
        if self.phase == RecoveryPhase::Corrective {
            self.phase = RecoveryPhase::Exhausted;
        }
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.phase == RecoveryPhase::Exhausted
    }

    /// Successful execution can clear only a correction of a rejected operation.
    /// A read/verifier for another operation must not replenish this episode.
    pub(crate) fn succeeded(&mut self, tool_name: &str, arguments: &Value, planning_active: bool) {
        let corrected = self.rejected_calls.iter().any(|rejected| {
            if rejected.tool_name != tool_name {
                return false;
            }
            let Some(previous) = rejected.arguments.as_ref() else {
                return true;
            };
            operation_matches(tool_name, previous, arguments, planning_active)
        });
        if corrected && !self.exhausted() {
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preflight_recovery_bounds_one_corrective_turn_and_canonical_identity() {
        let mut episode = PreflightRecovery::default();
        let rejected = json!({"action":"list","index":2,"max_output_tokens":100});
        assert!(!episode.reject("task_tracker", ValidationCategory::Arguments, Some(&rejected), 2));
        episode.synthesis();
        assert!(episode.corrective_pending());
        episode.begin_corrective_turn();
        let reordered: Value = serde_json::from_str(r#"{"max_output_tokens":5,"index":2,"action":"list"}"#).unwrap();
        assert!(episode.reject("task_tracker", ValidationCategory::Arguments, Some(&reordered), 2));
        assert!(episode.exhausted());
        episode.synthesis();
        assert!(!episode.corrective_pending());
    }

    #[test]
    fn preflight_changed_calls_stay_bounded_and_unrelated_success_does_not_clear() {
        let mut episode = PreflightRecovery::default();
        episode.reject("task_tracker", ValidationCategory::Arguments, Some(&json!({"action":"list","index":2})), 2);
        episode.synthesis();
        episode.begin_corrective_turn();
        episode.succeeded("exec_command", &json!({"cmd":"cargo check --locked"}), false);
        episode.succeeded("task_tracker", &json!({"action":"update","index":1,"status":"completed"}), false);
        assert!(!episode.corrective_pending());
        assert!(!episode.reject(
            "task_tracker",
            ValidationCategory::Arguments,
            Some(&json!({"action":"list","index":3})),
            2
        ));
        episode.reject("task_tracker", ValidationCategory::MalformedJson, None, 2);
        assert_eq!(episode.rejected_calls.len(), 2);
        episode.finish_corrective_turn();
        assert!(episode.exhausted());
    }

    #[test]
    fn preflight_successful_correction_clears_only_the_validation_episode() {
        let mut episode = PreflightRecovery::default();
        episode.reject("task_tracker", ValidationCategory::Arguments, Some(&json!({"action":"list","items":[]})), 2);
        episode.synthesis();
        episode.begin_corrective_turn();
        episode.succeeded("task_tracker", &json!({"action":"list"}), false);
        assert_eq!(episode.phase, RecoveryPhase::Collecting);
        assert!(episode.rejected_calls.is_empty());
    }

    #[test]
    fn preflight_tracker_corrections_repair_invalid_fields_and_preserve_valid_targets() {
        let mut episode = PreflightRecovery::default();
        episode.reject(tools::TASK_TRACKER, ValidationCategory::Arguments, Some(&json!({"action":"lst"})), 6);
        episode.succeeded(tools::TASK_TRACKER, &json!({"action":"list"}), false);
        assert!(episode.is_clear());

        episode.reject(
            tools::TASK_TRACKER,
            ValidationCategory::Arguments,
            Some(&json!({"action":"update","index":1})),
            6,
        );
        episode.succeeded(tools::TASK_TRACKER, &json!({"action":"update","index":2,"status":"completed"}), false);
        assert!(!episode.is_clear());
        episode.succeeded(
            tools::TASK_TRACKER,
            &json!({"action":"update","index_path":"01","status":"completed"}),
            false,
        );
        assert!(episode.is_clear());

        episode.reject(
            tools::TASK_TRACKER,
            ValidationCategory::Arguments,
            Some(&json!({"action":"update","index_path":"01.02"})),
            6,
        );
        episode.succeeded(tools::TASK_TRACKER, &json!({"action":"update","index_path":"1.3","status":"pending"}), true);
        assert!(!episode.is_clear());
        episode.succeeded(tools::TASK_TRACKER, &json!({"action":"update","index_path":"1.2","status":"pending"}), true);
        assert!(episode.is_clear());

        episode.reject(
            tools::TASK_TRACKER,
            ValidationCategory::Arguments,
            Some(&json!({"action":"update","index":0})),
            6,
        );
        episode.succeeded(tools::TASK_TRACKER, &json!({"action":"update","index":1,"status":"completed"}), true);
        assert!(episode.is_clear(), "planning index zero is invalid and can be repaired");
    }
}

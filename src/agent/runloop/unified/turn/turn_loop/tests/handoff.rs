use super::*;

#[tokio::test]
async fn interrupted_provider_follow_up_stops_after_completed_tool_without_recovery() {
    struct InterruptedProvider {
        requests: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl uni::LLMProvider for InterruptedProvider {
        fn name(&self) -> &str {
            "openai"
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        async fn generate(&self, request: uni::LLMRequest) -> Result<uni::LLMResponse, uni::LLMError> {
            let call = self.requests.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                return Err(uni::LLMError::Provider {
                    message: "Provider error: Interrupted by user".to_owned(),
                    metadata: None,
                });
            }
            let path = if call == 0 {
                "completed.txt"
            } else {
                "late-mutation.txt"
            };
            Ok(uni::LLMResponse {
                content: None,
                model: request.model,
                tool_calls: Some(vec![uni::ToolCall::function(
                    "write".to_owned(),
                    tool_names::APPLY_PATCH.to_owned(),
                    json!({"input": format!("*** Begin Patch\n*** Add File: {path}\n+retained\n*** End Patch\n")})
                        .to_string(),
                )]),
                usage: None,
                finish_reason: uni::FinishReason::Stop,
                reasoning: None,
                reasoning_details: None,
                organization_id: None,
                request_id: None,
                tool_references: Vec::new(),
                compaction: None,
            })
        }
        fn supported_models(&self) -> Vec<String> {
            vec!["noop-model".to_owned()]
        }
        fn validate_request(&self, _: &uni::LLMRequest) -> Result<(), uni::LLMError> {
            Ok(())
        }
    }
    let requests = Arc::new(AtomicUsize::new(0));
    let mut backing = TestTurnProcessingBacking::new(8).await;
    let harness_path = backing.enable_harness_emitter();
    backing.set_provider(Box::new(InterruptedProvider { requests: requests.clone() }));
    let mut history = vec![uni::Message::user("write the file".to_owned())];
    let context = backing.turn_loop_context();
    let emitter = context.harness_emitter.cloned().unwrap();
    let outcome = run_turn_loop(&mut history, context).await.expect("cancelled turn");
    emitter.finish().await.unwrap();
    assert!(matches!(outcome.result, TurnLoopResult::Cancelled));
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    assert_eq!(fs::read_to_string(backing.workspace_path().join("completed.txt")).unwrap(), "retained\n");
    assert!(!backing.workspace_path().join("late-mutation.txt").exists());
    assert!(
        !history
            .iter()
            .any(|m| m.role == uni::MessageRole::System && m.content.as_text().contains(POST_TOOL_RESUME_DIRECTIVE))
    );
    assert!(!outcome.final_response_was_fallback);
    let events = fs::read_to_string(harness_path).unwrap();
    assert_eq!(events.matches("turn.failed").count(), 1, "{events}");
    assert!(!events.contains("turn.blocked") && !events.contains("full_auto_continuation"));
}

#[test]
fn provider_cancellation_and_shared_exit_precede_other_errors() {
    let state = crate::agent::runloop::unified::state::CtrlCState::new();
    let error = anyhow::anyhow!("Interrupted by user");
    assert!(matches!(provider_stop_outcome(&state, &error), Some(TurnLoopResult::Cancelled)));
    assert!(provider_stop_outcome(&state, &anyhow::anyhow!("invalid schema")).is_none());
    state.request_local_cancel();
    assert!(matches!(
        provider_stop_outcome(&state, &anyhow::anyhow!("invalid schema")),
        Some(TurnLoopResult::Cancelled)
    ));
    state.request_exit();
    assert!(matches!(provider_stop_outcome(&state, &error), Some(TurnLoopResult::Exit)));
}

#[test]
fn current_turn_preserve_index_keeps_user_request_before_transient_notes() {
    let history = vec![
        uni::Message::assistant("older response".to_string()),
        uni::Message::user("apply the requested fix".to_string()),
        uni::Message::system("transient recovery note".to_string()),
    ];

    assert_eq!(current_turn_preserve_index(&history, history.len()), 1);
}

#[test]
fn recovery_synthesis_fallback_says_no_tool_call_was_applied() {
    assert!(RECOVERY_SYNTHESIS_FALLBACK_FINAL_ANSWER.contains("no tool call applied"));
}

#[test]
fn approved_plan_handoff_bypasses_final_response_guard() {
    let approved_handoff = TurnLoopResult::Completed { plan_approved_execution_pending: true };
    let ordinary_completion = TurnLoopResult::Completed { plan_approved_execution_pending: false };

    assert!(!completed_turn_requires_final_response(&approved_handoff, true));
    assert!(!completed_turn_requires_final_response(&approved_handoff, false));
    assert!(completed_turn_requires_final_response(&ordinary_completion, false));
    // Plan-entry handoff completes with `plan_approved_execution_pending: false`
    // but is still a control-flow turn: the tool result is its terminal output.
    assert!(!completed_turn_requires_final_response(&ordinary_completion, true));
}

#[test]
fn plan_entry_handoff_is_not_approved_plan_execution() {
    use super::is_plan_entry_handoff;

    assert!(is_plan_entry_handoff("plan"));
    assert!(is_plan_entry_handoff("Plan"));
    assert!(!is_plan_entry_handoff("build"));
    assert!(!is_plan_entry_handoff("auto"));
}

#[test]
fn deferred_plan_entry_switch_counts_as_primary_agent_handoff() {
    use super::is_primary_agent_handoff;
    use crate::agent::runloop::unified::planning_workflow::{PlanExecutionContext, PlanExecutionTarget};

    let none: Option<String> = None;
    let plan = Some("plan".to_string());
    let target = PlanExecutionTarget::build(PlanExecutionContext::Current, false);
    assert!(is_primary_agent_handoff(&plan, &None, false));
    assert!(
        is_primary_agent_handoff(&none, &None, true),
        "deferred plan entry must exempt the final-response guard"
    );
    assert!(!is_primary_agent_handoff(&none, &None, false));
    // A stronger handoff already owns the boundary even without the switch.
    assert!(is_primary_agent_handoff(&none, &Some(target), false));
}

#[test]
fn completed_fallback_reason_preserves_planning_specificity() {
    use super::completed_fallback_reason;

    let planning = completed_fallback_reason(true);
    assert!(
        planning.contains("planning remains active"),
        "planning fallback must name planning state: {planning}"
    );
    assert!(
        planning.contains("approval-ready plan"),
        "planning fallback must name approval readiness: {planning}"
    );

    let generic = completed_fallback_reason(false);
    assert!(generic.contains("recovery fallback"), "non-planning fallback must keep generic reason: {generic}");
    assert_ne!(planning, generic);
}

#[test]
fn planning_recovery_prompt_constants_keep_inspection_verify_examples() {
    use super::{
        PLANNING_COMPLETED_FALLBACK_RESPONSE, POST_TOOL_RECOVERY_REASON_PLAN_MODE,
        RECOVERY_TOOL_CALL_RETRY_DIRECTIVE_PLAN_MODE,
    };

    for (name, text) in [
        ("completed-fallback", PLANNING_COMPLETED_FALLBACK_RESPONSE),
        ("post-tool-plan-mode", POST_TOOL_RECOVERY_REASON_PLAN_MODE),
        ("tool-call-retry-plan-mode", RECOVERY_TOOL_CALL_RETRY_DIRECTIVE_PLAN_MODE),
    ] {
        assert!(text.contains("sed -n") && text.contains("grep -n"), "{name} missing inspection examples: {text}");
        assert!(text.contains("git diff --check"), "{name} missing invalid VCS example: {text}");
    }
}

#[tokio::test]
async fn approved_plan_handoff_without_assistant_response_stays_completed() {
    let mut backing = TestTurnProcessingBacking::new(4).await;
    backing.activate_planning_for_test();
    backing.persist_plan_for_test(STREAMED_VALID_PLAN).await;
    let harness_path = backing.enable_harness_emitter();

    let mut history = vec![uni::Message::user("yes".to_string())];
    let outcome = run_turn_loop(&mut history, backing.turn_loop_context())
        .await
        .expect("approved plan should hand off without a synthesis response");

    assert!(outcome.plan_approved_execution_pending);
    assert!(matches!(outcome.result, TurnLoopResult::Completed { plan_approved_execution_pending: true }));
    assert!(!outcome.final_response_was_fallback);

    let harness = fs::read_to_string(harness_path).expect("read approved-plan handoff events");
    let events = harness
        .lines()
        .map(|line| {
            serde_json::from_str::<VersionedThreadEvent>(line)
                .expect("approved-plan handoff harness output should be versioned")
                .into_event()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ThreadEvent::TurnCompleted(_)))
            .count(),
        1,
        "approved-plan handoff must emit exactly one control-flow completion"
    );
    assert!(
        !events.iter().any(|event| matches!(event, ThreadEvent::TurnFailed(_))),
        "approved-plan handoff must not be reported as blocked"
    );
    assert!(
        !events.iter().any(|event| {
            matches!(event, ThreadEvent::ItemCompleted(item) if matches!(item.item.details, ThreadItemDetails::AgentMessage(_)))
        }),
        "approved-plan handoff must not synthesize a final assistant item"
    );
}

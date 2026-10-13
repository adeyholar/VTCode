//! Chat Completions and Responses request builders for OpenAI-compatible APIs.
//!
//! Model policy and Responses history shaping live in private child modules.

use crate::error_display;
use crate::provider;
use crate::providers::common::serialize_message_content_openai_for_model;
use crate::rig_adapter::RigProviderCapabilities;
use hashbrown::HashSet;
use rig::providers::openai::responses_api::{
    AdditionalParameters as RigResponsesAdditionalParameters, Include as RigResponsesInclude,
};
use serde_json::{Value, json};
use vtcode_config::core::OpenAIHostedShellConfig;
use vtcode_config::models::{Provider as ModelProvider, ProviderModelSupport};
use vtcode_config::types::ReasoningEffortLevel;

use super::tool_serialization;
use super::types::{MAX_COMPLETION_TOKENS_FIELD, OpenAIResponsesPayload};
use crate::providers::shared::split_dynamic_prompt_suffix;

mod history;
mod model_policy;

use history::{augment_openai_instructions, build_responses_item_history};
use model_policy::{
    allows_sampling_parameters, default_reasoning_effort_for_model, default_text_verbosity_for_model, is_gpt6_model,
    is_gpt56_model, supports_text_verbosity,
};

/// Default `prompt_cache_options.ttl` for GPT-5.6-family Responses requests.
/// Currently the only value OpenAI accepts; sent explicitly so cache intent
/// does not rely on the implicit default alone.
const DEFAULT_GPT56_PROMPT_CACHE_TTL: &str = "30m";

pub(crate) struct ChatRequestContext<'a> {
    pub model: &'a str,
    pub is_native_openai: bool,
    pub supports_tools: bool,
    pub supports_parallel_tool_config: bool,
    pub supports_temperature: bool,
    pub prompt_cache_key: Option<&'a str>,
    pub default_service_tier: Option<&'a str>,
}

pub(crate) struct ResponsesRequestContext<'a> {
    pub supports_tools: bool,
    pub supports_allowed_tools: bool,
    pub supports_parallel_tool_config: bool,
    pub supports_temperature: bool,
    pub supports_reasoning_effort: bool,
    pub supported_reasoning_efforts: &'a [&'a str],
    pub supports_reasoning: bool,
    pub is_responses_api_model: bool,
    pub include_max_output_tokens: bool,
    pub include_output_types: bool,
    pub include_sampling_parameters: bool,
    pub force_response_store_false: bool,
    pub include_assistant_phase: bool,
    pub prompt_cache_key: Option<&'a str>,
    pub include_prompt_cache_retention: bool,
    pub prompt_cache_retention: Option<&'a str>,
    /// Emit explicit `prompt_cache_breakpoint` markers on the stable prefix.
    /// Only native api.openai.com honors them: third-party
    /// Responses-compatible endpoints (e.g. Merge Gateway via a base-URL
    /// override) reject the field with 400 `invalid_parameter`, and the
    /// ChatGPT subscription backend rejects it with 400
    /// `prompt_cache_breakpoint is not supported on this model` (observed
    /// live on `gpt-6.1-sol`). Implicit breakpoints still cover the latest
    /// message on those backends.
    pub include_explicit_cache_breakpoints: bool,
    pub default_service_tier: Option<&'a str>,
    pub default_response_store: Option<bool>,
    pub default_responses_include: Option<&'a [String]>,
    pub include_encrypted_reasoning: bool,
    pub hosted_shell: Option<&'a OpenAIHostedShellConfig>,
    pub include_structured_history_in_input: bool,
    pub preserve_structured_history_on_replay: bool,
    pub preserve_assistant_phase_on_replay: bool,
    pub reasoning_context: Option<&'a str>,
    pub safety_identifier: Option<&'a str>,
}

fn push_unique_include(include_values: &mut Vec<String>, field: &str) {
    let field = field.trim();
    if field.is_empty() || include_values.iter().any(|value| value == field) {
        return;
    }

    include_values.push(field.to_string());
}

fn rig_include_for_field(field: &str) -> Option<RigResponsesInclude> {
    match field {
        "file_search_call.results" => Some(RigResponsesInclude::FileSearchCallResults),
        "message.input_image.image_url" => Some(RigResponsesInclude::MessageInputImageImageUrl),
        "computer_call.output.image_url" => Some(RigResponsesInclude::ComputerCallOutputOutputImageUrl),
        "reasoning.encrypted_content" => Some(RigResponsesInclude::ReasoningEncryptedContent),
        "code_interpreter_call.outputs" => Some(RigResponsesInclude::CodeInterpreterCallOutputs),
        _ => None,
    }
}

fn responses_include_value(field: &str) -> Value {
    rig_include_for_field(field)
        .and_then(|include| serde_json::to_value(include).ok())
        .unwrap_or_else(|| json!(field))
}

fn merge_typed_responses_parameters(openai_request: &mut Value, params: RigResponsesAdditionalParameters) {
    let Ok(Value::Object(fields)) = serde_json::to_value(params) else {
        return;
    };
    let Some(request) = openai_request.as_object_mut() else {
        return;
    };

    request.extend(fields);
}

fn openai_responses_allowed_tools_choice(tool_choice: &provider::ToolChoice, stable_tools: &[Value]) -> Option<Value> {
    let provider::ToolChoice::AllowedTools(choice) = tool_choice else {
        return None;
    };
    if choice.tools.is_empty() {
        return None;
    }

    let active_names: HashSet<&str> = choice.tools.iter().map(String::as_str).collect();
    let tools = stable_tools
        .iter()
        .filter_map(|tool| {
            let tool_type = tool.get("type")?.as_str()?;
            let name = tool.get("name").and_then(Value::as_str).unwrap_or(tool_type);
            if !active_names.contains(name) {
                return None;
            }
            let mut reference = json!({"type": tool_type});
            for field in ["name", "server_label"] {
                if let Some(value) = tool.get(field) {
                    reference[field] = value.clone();
                }
            }
            Some(reference)
        })
        .collect::<Vec<_>>();
    if tools.is_empty() {
        return None;
    }

    Some(json!({
        "type": "allowed_tools",
        "mode": choice.mode.as_str(),
        "tools": tools,
    }))
}

fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn build_chat_request(
    request: &provider::LLMRequest,
    ctx: &ChatRequestContext<'_>,
) -> Result<Value, provider::LLMError> {
    for message in request.messages.iter() {
        if let provider::MessageContent::Parts(parts) = &message.content {
            for part in parts {
                if let provider::ContentPart::File { file_url: Some(_), .. } = part {
                    let formatted_error = error_display::format_llm_error(
                        "OpenAI",
                        "Chat Completions does not support file_url inputs; use Responses API or file_id/file_data",
                    );
                    return Err(provider::LLMError::InvalidRequest { message: formatted_error, metadata: None });
                }
            }
        }
    }

    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    let mut active_tool_call_ids: HashSet<String> = HashSet::with_capacity(16);

    // Stable/dynamic split mirrors the Responses path: volatile sections move
    // to a trailing system message so per-turn runtime content stops rewriting
    // the cached leading prefix. Without dynamics this is byte-identical to
    // the legacy single system message.
    let mut trailing_system: Option<String> = None;
    if let Some(system_prompt) = &request.system_prompt {
        let (stable, dynamic) = split_dynamic_prompt_suffix(system_prompt.as_ref());
        if let Some(dynamic) = dynamic.filter(|text| !text.trim().is_empty()) {
            trailing_system = Some(dynamic);
            if !stable.trim().is_empty() {
                let stable = augment_openai_instructions(&request.model, stable);
                messages.push(json!({
                    "role": vtcode_config::constants::message_roles::SYSTEM,
                    "content": stable
                }));
            }
        } else {
            let system_prompt = augment_openai_instructions(&request.model, system_prompt.to_string());
            messages.push(json!({
                "role": vtcode_config::constants::message_roles::SYSTEM,
                "content": system_prompt
            }));
        }
    }

    for msg in request.messages.iter() {
        let role = msg.role.as_openai_str();
        let mut message = json!({
            "role": role,
            "content": serialize_message_content_openai_for_model(msg, &request.model)
        });
        let mut skip_message = false;

        if msg.role == provider::MessageRole::Assistant
            && let Some(tool_calls) = &msg.tool_calls
            && !tool_calls.is_empty()
        {
            let tool_calls_json: Vec<Value> = tool_calls
                .iter()
                .filter_map(|tc| {
                    tc.function.as_ref().map(|func| {
                        active_tool_call_ids.insert(tc.id.clone());
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": func.name,
                                "arguments": func.arguments
                            }
                        })
                    })
                })
                .collect();

            message["tool_calls"] = Value::Array(tool_calls_json);
        }

        if msg.role == provider::MessageRole::Tool {
            match &msg.tool_call_id {
                Some(tool_call_id) if active_tool_call_ids.contains(tool_call_id) => {
                    message["tool_call_id"] = Value::String(tool_call_id.clone());
                    active_tool_call_ids.remove(tool_call_id);
                }
                Some(_) | None => {
                    skip_message = true;
                }
            }
        }

        if !skip_message {
            messages.push(message);
        }
    }

    if let Some(dynamic) = trailing_system.filter(|text| !text.trim().is_empty()) {
        messages.push(json!({
            "role": vtcode_config::constants::message_roles::SYSTEM,
            "content": dynamic
        }));
    }

    if messages.is_empty() {
        let formatted_error = error_display::format_llm_error("OpenAI", "No messages provided");
        return Err(provider::LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    let mut openai_request = json!({
        "model": request.model,
        "messages": messages,
        "stream": request.stream
    });
    let effective_reasoning_effort = request
        .reasoning_effort
        .or_else(|| default_reasoning_effort_for_model(&request.model));

    let max_tokens_field = if !ctx.is_native_openai {
        "max_tokens"
    } else {
        MAX_COMPLETION_TOKENS_FIELD
    };

    if let Some(max_tokens) = request.max_tokens {
        openai_request[max_tokens_field] = json!(max_tokens);
    }

    if let Some(temperature) = request.temperature
        && ctx.supports_temperature
        && allows_sampling_parameters(&request.model, effective_reasoning_effort)
    {
        openai_request["temperature"] = Value::Number(crate::providers::common::float_to_json_number(temperature)?);
    }

    if ctx.supports_temperature && allows_sampling_parameters(&request.model, effective_reasoning_effort) {
        if let Some(top_p) = request.top_p {
            openai_request["top_p"] = Value::Number(crate::providers::common::float_to_json_number(top_p)?);
        }
        if let Some(presence_penalty) = request.presence_penalty {
            openai_request["presence_penalty"] =
                Value::Number(crate::providers::common::float_to_json_number(presence_penalty)?);
        }
        if let Some(frequency_penalty) = request.frequency_penalty {
            openai_request["frequency_penalty"] =
                Value::Number(crate::providers::common::float_to_json_number(frequency_penalty)?);
        }
    }

    if ModelProvider::OpenAI.supports_service_tier(&request.model)
        && let Some(service_tier) = trimmed_non_empty(request.service_tier.as_deref().or(ctx.default_service_tier))
    {
        openai_request["service_tier"] = json!(service_tier);
    }

    if let Some(prompt_cache_key) = trimmed_non_empty(ctx.prompt_cache_key) {
        openai_request["prompt_cache_key"] = json!(prompt_cache_key);
    }

    if ctx.supports_tools
        && let Some(tools) = &request.tools
        && let Some(serialized) = tool_serialization::serialize_tools(tools, ctx.model)
    {
        openai_request["tools"] = serialized;

        let has_custom_tool = tools.iter().any(|tool| tool.tool_type == "custom");
        if has_custom_tool {
            openai_request["parallel_tool_calls"] = Value::Bool(false);
        }

        if let Some(tool_choice) = &request.tool_choice {
            openai_request["tool_choice"] = tool_choice.to_provider_format("openai");
        }

        if request.parallel_tool_calls.is_some()
            && openai_request.get("parallel_tool_calls").is_none()
            && let Some(parallel) = request.parallel_tool_calls
        {
            openai_request["parallel_tool_calls"] = Value::Bool(parallel);
        }

        if ctx.supports_parallel_tool_config
            && let Some(config) = &request.parallel_tool_config
            && let Ok(config_value) = serde_json::to_value(config)
        {
            openai_request["parallel_tool_config"] = config_value;
        }
    }

    Ok(openai_request)
}

pub(crate) fn build_responses_request(
    request: &provider::LLMRequest,
    ctx: &ResponsesRequestContext<'_>,
) -> Result<Value, provider::LLMError> {
    let responses_payload = build_responses_item_history(request, ctx)?;
    build_responses_request_from_history(request, ctx, responses_payload)
}

/// Shared normal HTTP/SSE Responses JSON boundary.
///
/// Rig 0.40 typed request fields are used here for compatible state such as
/// `store`, `prompt_cache_key`, `prompt_cache_retention`, and known `include`
/// enum values. The final JSON remains custom because Rig lacks typed coverage
/// for VTCode's `output_types`, nested `sampling_parameters`,
/// `context_management`, text verbosity, and provider-specific tool payloads.
/// Protective tests: `openai_request_builder_serialises_rig_typed_state_fields`,
/// `chatgpt_backend_forces_store_false_and_omits_output_sampling_cache`, and
/// `responses_payload_includes_prompt_cache_retention_for_native_openai`.
/// Remove this boundary when Rig exposes those fields with identical streaming
/// and request JSON parity. Normal HTTP/SSE requests deliberately never set
/// `previous_response_id`; WebSocket continuation has its own transport-local
/// path.
fn build_responses_request_from_history(
    request: &provider::LLMRequest,
    ctx: &ResponsesRequestContext<'_>,
    responses_payload: OpenAIResponsesPayload,
) -> Result<Value, provider::LLMError> {
    let input = responses_payload.input;
    let instructions = responses_payload.instructions;
    if input.is_empty() {
        let formatted_error = error_display::format_llm_error("OpenAI", "No messages provided for Responses API");
        return Err(provider::LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    let mut openai_request = json!({
        "model": request.model,
        "input": input,
        "stream": request.stream,
    });
    let effective_reasoning_effort = request
        .reasoning_effort
        .or_else(|| default_reasoning_effort_for_model(&request.model));

    if ctx.include_max_output_tokens
        && let Some(max_tokens) = request.max_tokens
    {
        // Responses requires at least 16, including one-word auxiliary probes.
        openai_request["max_output_tokens"] = json!(max_tokens.max(16));
    }

    if ctx.include_output_types {
        // `output_types` constrains which native item types GPT-5 may emit.
        let mut output_types = vec!["message", "tool_call"];
        if ctx.hosted_shell.is_some() {
            output_types.push("shell_call");
        }
        openai_request["output_types"] = json!(output_types);
    }

    if let Some(instructions) = instructions
        && !instructions.trim().is_empty()
    {
        openai_request["instructions"] = json!(instructions);
    }

    let mut typed_parameters = RigResponsesAdditionalParameters::default();

    if ModelProvider::OpenAI.supports_service_tier(&request.model)
        && let Some(service_tier) = trimmed_non_empty(request.service_tier.as_deref().or(ctx.default_service_tier))
    {
        openai_request["service_tier"] = json!(service_tier);
    }

    if ctx.force_response_store_false {
        typed_parameters.store = Some(false);
    } else if let Some(store) = request.response_store.or(ctx.default_response_store) {
        typed_parameters.store = Some(store);
    }

    if let Some(prompt_cache_key) = trimmed_non_empty(ctx.prompt_cache_key) {
        typed_parameters.prompt_cache_key = Some(prompt_cache_key.to_string());
    }

    let cache_ttl = vtcode_config::models::model_catalog_entry("openai", &request.model)
        .and_then(|entry| entry.prompt_cache_ttl)
        .and_then(|ttl| trimmed_non_empty(Some(ttl)));
    // GPT-5.6+ uses `prompt_cache_options.ttl` ("30m" is currently the only
    // supported value); the legacy `prompt_cache_retention` field is
    // deprecated for these models. No catalog entry sets a TTL today, so
    // default GPT-5.6-family Responses requests to "30m" to document caching
    // intent explicitly instead of relying on the implicit default alone.
    let cache_ttl = cache_ttl.or_else(|| {
        ((is_gpt56_model(&request.model) || is_gpt6_model(&request.model))
            && ctx.include_prompt_cache_retention
            && ctx.is_responses_api_model)
            .then_some(DEFAULT_GPT56_PROMPT_CACHE_TTL)
    });
    if let Some(ttl) = cache_ttl
        && ctx.include_prompt_cache_retention
        && ctx.is_responses_api_model
    {
        openai_request["prompt_cache_options"] = json!({ "ttl": ttl });
    } else if ctx.include_prompt_cache_retention
        && ctx.is_responses_api_model
        && let Some(retention) = trimmed_non_empty(ctx.prompt_cache_retention)
    {
        typed_parameters.prompt_cache_retention = Some(retention.to_string());
    }

    merge_typed_responses_parameters(&mut openai_request, typed_parameters);

    let mut include_values = Vec::new();
    if let Some(include_fields) = request.responses_include.as_deref().or(ctx.default_responses_include) {
        for field in include_fields {
            push_unique_include(&mut include_values, field);
        }
    }
    if ctx.include_encrypted_reasoning {
        push_unique_include(&mut include_values, "reasoning.encrypted_content");
    }
    let supports_logprobs = vtcode_config::models::model_catalog_entry("openai", &request.model)
        .is_none_or(|entry| entry.supports_logprobs);
    if !supports_logprobs {
        include_values.retain(|field| field != "message.output_text.logprobs");
    }
    if !include_values.is_empty() {
        openai_request["include"] =
            Value::Array(include_values.iter().map(|field| responses_include_value(field)).collect());
    }

    if let Some(context_management) = &request.context_management {
        openai_request["context_management"] = context_management.clone();
    }

    let mut sampling_parameters = json!({});
    let mut has_sampling = false;

    if let Some(temperature) = request.temperature
        && ctx.supports_temperature
        && allows_sampling_parameters(&request.model, effective_reasoning_effort)
    {
        sampling_parameters["temperature"] =
            Value::Number(crate::providers::common::float_to_json_number(temperature)?);
        has_sampling = true;
    }

    if let Some(top_p) = request.top_p
        && allows_sampling_parameters(&request.model, effective_reasoning_effort)
    {
        sampling_parameters["top_p"] = Value::Number(crate::providers::common::float_to_json_number(top_p)?);
        has_sampling = true;
    }

    if let Some(presence_penalty) = request.presence_penalty
        && allows_sampling_parameters(&request.model, effective_reasoning_effort)
    {
        sampling_parameters["presence_penalty"] =
            Value::Number(crate::providers::common::float_to_json_number(presence_penalty)?);
        has_sampling = true;
    }

    if let Some(frequency_penalty) = request.frequency_penalty
        && allows_sampling_parameters(&request.model, effective_reasoning_effort)
    {
        sampling_parameters["frequency_penalty"] =
            Value::Number(crate::providers::common::float_to_json_number(frequency_penalty)?);
        has_sampling = true;
    }

    if ctx.include_sampling_parameters && has_sampling {
        openai_request["sampling_parameters"] = sampling_parameters;
    }

    if ctx.supports_tools
        && let Some(tools) = &request.tools
        && let Some(serialized) = tool_serialization::serialize_tools_for_responses(tools, ctx.hosted_shell)
    {
        openai_request["tools"] = serialized;

        // Check if any tools are custom types - if so, disable parallel tool calls
        // as per GPT-5 specification: "custom tool type does NOT support parallel tool calling"
        let has_custom_tool = tools.iter().any(|tool| tool.tool_type == "custom");
        if has_custom_tool {
            // Override parallel tool calls to false if custom tools are present
            openai_request["parallel_tool_calls"] = Value::Bool(false);
        }

        // Only add tool_choice when tools are present. Native allowed-tools
        // filtering is advisory and derives its subset from the stable
        // catalogue above; unsupported backends/models degrade to regular
        // provider tool_choice values.
        if let Some(tool_choice) = &request.tool_choice {
            openai_request["tool_choice"] = if ctx.supports_allowed_tools {
                openai_responses_allowed_tools_choice(
                    tool_choice,
                    openai_request["tools"].as_array().map(Vec::as_slice).unwrap_or_default(),
                )
                .unwrap_or_else(|| tool_choice.to_provider_format("openai"))
            } else {
                tool_choice.to_provider_format("openai")
            };
        }

        // Only set parallel tool calls if not overridden due to custom tools
        if let Some(parallel) = request.parallel_tool_calls
            && openai_request.get("parallel_tool_calls").is_none()
        {
            openai_request["parallel_tool_calls"] = Value::Bool(parallel);
        }

        // Only add parallel_tool_config when tools are present
        if ctx.supports_parallel_tool_config
            && let Some(config) = &request.parallel_tool_config
            && let Ok(config_value) = serde_json::to_value(config)
        {
            openai_request["parallel_tool_config"] = config_value;
        }
    }

    if ctx.supports_reasoning_effort {
        if let Some(effort) = request.reasoning_effort {
            // The typed adapter validates native catalog models. A custom
            // OpenAI-compatible route can advertise effort support for a
            // model absent from the built-in catalog; that capability was
            // already resolved by the provider trait, so preserve its exact
            // value instead of applying native catalog validation.
            let payload = if ctx.is_responses_api_model {
                RigProviderCapabilities::new(ModelProvider::OpenAI, &request.model)
                    .reasoning_parameters_for_supported_efforts(effort, ctx.supported_reasoning_efforts)?
            } else {
                Some(json!({ "effort": effort.as_str() }))
            };
            if let Some(payload) = payload {
                openai_request["reasoning"] = payload;
            } else {
                openai_request["reasoning"] = json!({ "effort": effort.as_str() });
            }
        } else if openai_request.get("reasoning").is_none()
            && let Some(default_effort) = default_reasoning_effort_for_model(&request.model)
        {
            openai_request["reasoning"] = json!({ "effort": default_effort.as_str() });
        }
    }

    // Enable reasoning summaries if supported (OpenAI GPT-5 only)
    if ctx.supports_reasoning
        && let Some(map) = openai_request.as_object_mut()
    {
        let reasoning_value = map.entry("reasoning".to_string()).or_insert(json!({}));
        if let Some(reasoning_obj) = reasoning_value.as_object_mut() {
            reasoning_obj.entry("summary".to_string()).or_insert_with(|| json!("auto"));
            // Add reasoning.context for persisted reasoning (GPT-5.6+)
            if let Some(context) = ctx.reasoning_context {
                reasoning_obj.entry("context".to_string()).or_insert_with(|| json!(context));
            }
        }
    }

    // Add text formatting options for GPT-5 and compatible models, including verbosity and grammar
    let mut text_format = json!({});
    let mut has_format_options = false;

    if supports_text_verbosity(&request.model)
        && let Some(verbosity) = request.verbosity
    {
        text_format["verbosity"] = json!(verbosity.as_str());
        has_format_options = true;
    }

    // Add grammar constraint if tools include grammar definitions
    if let Some(ref tools) = request.tools {
        let grammar_tools: Vec<&provider::ToolDefinition> =
            tools.iter().filter(|tool| tool.tool_type == "grammar").collect();

        if !grammar_tools.is_empty() {
            // Use the first grammar definition found
            if let Some(grammar_tool) = grammar_tools.first()
                && let Some(ref grammar) = grammar_tool.grammar
            {
                text_format["format"] = json!({
                    "type": "grammar",
                    "syntax": grammar.syntax,
                    "definition": grammar.definition
                });
                has_format_options = true;
            }
        }
    }

    if !has_format_options && let Some(default_verbosity) = default_text_verbosity_for_model(&request.model) {
        text_format["verbosity"] = json!(default_verbosity.as_str());
        has_format_options = true;
    }

    if has_format_options {
        openai_request["text"] = text_format;
    }

    // Add safety_identifier for abuse detection (hashed, not logged)
    if let Some(safety_id) = trimmed_non_empty(ctx.safety_identifier)
        && let Some(map) = openai_request.as_object_mut()
    {
        map.entry("safety_identifier".to_string()).or_insert_with(|| json!(safety_id));
    }

    Ok(openai_request)
}

#[cfg(test)]
mod tests;

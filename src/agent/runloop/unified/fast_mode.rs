//! Shared native OpenAI fast-mode eligibility and session discovery.

use anyhow::Result;
use vtcode_config::models::{Provider, ProviderModelSupport};
use vtcode_config::{OpenAIServiceTier, VTCodeConfig};
use vtcode_core::config::types::AgentConfig;
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};

use super::state::SessionStats;
use crate::agent::runloop::model_picker::configured_openai_base_url;

pub(crate) fn supports_fast_mode(config: &AgentConfig, cfg: &VTCodeConfig) -> bool {
    config.provider.eq_ignore_ascii_case(Provider::OpenAI.as_ref())
        && config.openai_chatgpt_auth.is_none()
        && cfg.custom_provider(&config.provider).is_none()
        && Provider::OpenAI.supports_service_tier_value(&config.model, OpenAIServiceTier::Ultrafast)
        && configured_openai_base_url(Some(cfg))
            .is_none_or(|url| vtcode_core::llm::provider::is_standard_decisions_endpoint(&url))
}

/// Reevaluate at idle so startup and model/auth changes share one session tip.
pub(crate) fn maybe_render_fast_mode_suggestion(
    renderer: &mut AnsiRenderer,
    config: &AgentConfig,
    cfg: Option<&VTCodeConfig>,
    stats: &mut SessionStats,
) -> Result<()> {
    if cfg.is_some_and(|cfg| cfg.provider.openai.service_tier == Some(OpenAIServiceTier::Ultrafast)) {
        stats.dismiss_fast_mode_suggestion();
        return Ok(());
    }
    let applicable = renderer.supports_inline_ui()
        && !config.api_key.trim().is_empty()
        && cfg.is_some_and(|cfg| supports_fast_mode(config, cfg));
    if stats.take_fast_mode_suggestion(applicable) {
        renderer.line(
            MessageStyle::Info,
            &format!(
                "{} supports ultrafast processing with your OpenAI API key. Use /fast on for faster responses at higher cost; the choice is saved for future sessions.",
                config.model
            ),
        )?;
    }
    Ok(())
}

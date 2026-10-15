//! Transactional service-tier updates for `/fast`.

use anyhow::{Context, Result, bail};
use vtcode_config::{ConfigManager, OpenAIServiceTier};
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::config::types::AgentConfig;
use vtcode_core::llm::provider::{ContextWindowProvider, LLMProvider};
use vtcode_core::utils::ansi::MessageStyle;

use super::{SlashCommandContext, SlashCommandControl};
use crate::agent::runloop::unified::fast_mode::supports_fast_mode;
use crate::agent::runloop::unified::session_setup::create_provider_client;

pub(crate) async fn handle_toggle_fast_mode(
    ctx: SlashCommandContext<'_>,
    enable: Option<bool>,
) -> Result<SlashCommandControl> {
    let result = crate::main_helpers::load_workspace_config(&ctx.config.workspace)
        .and_then(|mut manager| apply_fast_mode(ctx.config, ctx.vt_cfg, ctx.provider_client, &mut manager, enable));
    match result {
        Ok(tier) => {
            ctx.session_stats.dismiss_fast_mode_suggestion();
            ctx.header_context.service_tier = tier
                .map(|tier| format!("{}{}", vtcode_config::constants::ui::HEADER_SERVICE_TIER_PREFIX, tier.as_str()));
            ctx.handle.set_header_context(ctx.header_context.clone());
            let message = if tier == Some(OpenAIServiceTier::Ultrafast) {
                format!(
                    "Ultrafast enabled for {} at higher cost. Saved for subsequent requests and sessions; the server reports the tier actually served.",
                    ctx.config.model
                )
            } else {
                "Fast mode off: project default service tier. Saved for subsequent requests and sessions.".to_string()
            };
            ctx.renderer.line(MessageStyle::Info, &message)?;
        }
        Err(err) => ctx
            .renderer
            .line(MessageStyle::Error, &format!("Could not change fast mode: {err:#}"))?,
    }
    Ok(SlashCommandControl::Continue)
}

fn apply_fast_mode(
    config: &AgentConfig,
    vt_cfg: &mut Option<VTCodeConfig>,
    provider_client: &mut Box<dyn LLMProvider>,
    manager: &mut ConfigManager,
    enable: Option<bool>,
) -> Result<Option<OpenAIServiceTier>> {
    let mut runtime_cfg = vt_cfg.as_ref().unwrap_or_else(|| manager.config()).clone();
    let current = runtime_cfg.provider.openai.service_tier;
    let enabled = enable.unwrap_or(current != Some(OpenAIServiceTier::Ultrafast));
    if enabled && !supports_fast_mode(config, &runtime_cfg) {
        bail!(
            "/fast on requires native OpenAI API-key authentication with gpt-6-astra or gpt-6.1-sol. Select one with /model, or use /fast off to clear the override."
        );
    }
    let tier = enabled.then_some(OpenAIServiceTier::Ultrafast);
    runtime_cfg.provider.openai.service_tier = tier;
    // Prepare before saving and install only after saving succeeds. Rebuilding
    // also clears a tier captured by the old client at session startup.
    let prepared = create_provider_client(config, Some(&runtime_cfg))?;
    let prepared = ContextWindowProvider::wrap(
        prepared,
        &config.model,
        Some(provider_client.effective_context_size(&config.model)),
    );
    let mut persisted_cfg = manager.config().clone();
    persisted_cfg.provider.openai.service_tier = tier;
    manager.save_config(&persisted_cfg).context("Failed to save service tier")?;
    *provider_client = prepared;
    *vt_cfg = Some(runtime_cfg);
    Ok(tier)
}

#[cfg(test)]
#[path = "fast/tests.rs"]
mod tests;

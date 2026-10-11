use crate::skills::cli_bridge::{CliToolBridge, CliToolConfig, discover_cli_tools};
use crate::skills::command_skills::{
    BuiltInCommandSkill, CommandSkillBackend, built_in_command_skill, find_command_skill_by_skill_name,
    merge_built_in_command_skill_contexts,
};
use crate::skills::container_validation::{
    ContainerSkillsValidator, ContainerValidationReport, ContainerValidationResult,
};
use crate::skills::discovery::{DiscoveryConfig, DiscoveryResult, SkillDiscovery};
use crate::skills::locations::{MAX_DISCOVERY_DEPTH, MAX_DISCOVERY_DIRS, discovery_dir_skipped};
use crate::skills::model::{SkillErrorInfo, SkillLoadOutcome, SkillMetadata, SkillScope};
use crate::skills::system::{install_system_skills, system_cache_root_dir};
use crate::skills::types::{Skill, SkillContext};
use crate::tools::error_messages::skill_ops;
use anyhow::{Context, Result};
use hashbrown::HashSet;
use serde::Deserialize;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{error, warn};
use vtcode_agent_plugins::LoadedPlugin;
use vtcode_commons::VtCodePaths;

// Config for loader
#[derive(Debug, Clone)]
pub struct SkillLoaderConfig {
    pub codex_home: PathBuf,
    pub cwd: PathBuf,
    pub project_root: Option<PathBuf>,
    pub include_bundled_system_skills: bool,
}

pub struct SkillRoot {
    pub path: PathBuf,
    pub scope: SkillScope,
    pub is_tool_root: bool,
    pub is_plugin_root: bool,
}

mod cache;
mod mentions;

pub use self::mentions::{SkillMentionDetectionOptions, detect_skill_mentions, detect_skill_mentions_with_options};

pub(crate) use self::cache::clear_lightweight_skill_metadata_cache;
use self::cache::{LightweightSkillCacheKey, cache_lightweight_skill_outcome, get_cached_lightweight_skill_outcome};

pub fn load_skills(config: &SkillLoaderConfig) -> SkillLoadOutcome {
    let home_dir = dirs::home_dir();
    load_skills_with_home_dir(config, home_dir.as_deref())
}

/// Lightweight metadata discovery that avoids parsing SKILL.md files.
/// Returns skill stubs with only name, description, and path (no manifest parsing).
/// This is much faster for listing available skills.
pub fn discover_skill_metadata_lightweight(config: &SkillLoaderConfig) -> SkillLoadOutcome {
    let home_dir = dirs::home_dir();
    discover_skill_metadata_lightweight_with_home_dir(config, home_dir.as_deref())
}

/// Internal helper that allows specifying an explicit home directory.
/// This is useful for testing to avoid picking up real user skills from ~/.agents/skills.
pub(crate) fn load_skills_with_home_dir(config: &SkillLoaderConfig, home_dir: Option<&Path>) -> SkillLoadOutcome {
    let mut outcome = SkillLoadOutcome::default();
    let roots = skill_roots_with_home_dir(config, home_dir);

    for root in roots {
        discover_skills_under_root(&root, &mut outcome);
    }

    add_system_cli_tools(&mut outcome);
    dedup_and_sort(&mut outcome);
    filter_disabled_skills(&mut outcome, home_dir);
    outcome
}

/// Internal helper for lightweight discovery with explicit home directory.
/// Useful for hermetic tests.
fn discover_skill_metadata_lightweight_with_home_dir(
    config: &SkillLoaderConfig,
    home_dir: Option<&Path>,
) -> SkillLoadOutcome {
    let cache_key = LightweightSkillCacheKey::new(config, home_dir);
    if let Some(cached) = get_cached_lightweight_skill_outcome(&cache_key) {
        return cached;
    }

    let outcome = discover_skill_metadata_lightweight_uncached(config, home_dir);
    cache_lightweight_skill_outcome(cache_key, &outcome);
    outcome
}

fn discover_skill_metadata_lightweight_uncached(
    config: &SkillLoaderConfig,
    home_dir: Option<&Path>,
) -> SkillLoadOutcome {
    let mut outcome = SkillLoadOutcome::default();
    let roots = skill_roots_with_home_dir(config, home_dir);

    for root in roots {
        discover_metadata_under_root(&root, &mut outcome);
    }

    add_system_cli_tools(&mut outcome);
    dedup_and_sort(&mut outcome);
    filter_disabled_skills(&mut outcome, home_dir);
    outcome
}

fn add_system_cli_tools(outcome: &mut SkillLoadOutcome) {
    if let Ok(system_tools) = discover_cli_tools() {
        for tool in system_tools {
            if let Ok(skill) = tool_config_to_metadata(&tool, SkillScope::System) {
                outcome.skills.push(skill);
            }
        }
    }
}

fn dedup_and_sort(outcome: &mut SkillLoadOutcome) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut shadowed: Vec<String> = Vec::new();
    outcome.skills.retain(|skill| {
        if seen.insert(skill.name.clone()) {
            return true;
        }
        // Agent Skills interop: same-name skills must not silently replace one
        // another. First discovery wins (project roots precede user roots);
        // warn so the shadowing is visible instead of silent.
        shadowed.push(format!("{} ({})", skill.name, skill.path.display()));
        false
    });
    if !shadowed.is_empty() {
        tracing::warn!(
            shadowed = ?shadowed,
            "duplicate skill names shadowed by earlier discovery; only the first occurrence is loaded"
        );
    }
    outcome.skills.sort_by(|a, b| a.name.cmp(&b.name));
}

#[derive(Debug, Default, Deserialize)]
struct CodexConfig {
    #[serde(default)]
    skills: CodexSkillsConfig,
}

#[derive(Debug, Default, Deserialize)]
struct CodexSkillsConfig {
    #[serde(default)]
    config: Vec<CodexSkillToggle>,
}

#[derive(Debug, Deserialize)]
struct CodexSkillToggle {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_skill_toggle_enabled")]
    enabled: bool,
}

#[derive(Debug, Default)]
struct DisabledSkillSelectors {
    paths: HashSet<PathBuf>,
    names: HashSet<String>,
}

fn default_skill_toggle_enabled() -> bool {
    true
}

fn filter_disabled_skills(outcome: &mut SkillLoadOutcome, home_dir: Option<&Path>) {
    let disabled = disabled_skill_selectors(home_dir);
    if disabled.paths.is_empty() && disabled.names.is_empty() {
        return;
    }

    outcome.skills.retain(|skill| {
        let canonical = dunce::canonicalize(&skill.path).unwrap_or_else(|_| skill.path.clone());
        !disabled.paths.contains(&canonical) && !disabled.names.contains(&skill.name)
    });
}

fn disabled_skill_selectors(home_dir: Option<&Path>) -> DisabledSkillSelectors {
    let Some(home_dir) = home_dir else {
        return DisabledSkillSelectors::default();
    };

    let config_path = home_dir.join(".codex").join("config.toml");
    let Ok(content) = fs::read_to_string(&config_path) else {
        return DisabledSkillSelectors::default();
    };
    let Ok(config) = toml::from_str::<CodexConfig>(&content) else {
        return DisabledSkillSelectors::default();
    };

    let mut selectors = DisabledSkillSelectors::default();
    for entry in config.skills.config.into_iter().filter(|entry| !entry.enabled) {
        if let Some(path) = entry.path {
            selectors.paths.insert(dunce::canonicalize(&path).unwrap_or(path));
        }
        if let Some(name) = entry.name.map(|name| name.trim().to_string())
            && !name.is_empty()
        {
            selectors.names.insert(name);
        }
    }

    selectors
}

fn skill_roots_with_home_dir(config: &SkillLoaderConfig, home_dir: Option<&Path>) -> Vec<SkillRoot> {
    let mut roots = Vec::new();

    for repo_dir in repo_skill_search_dirs(config) {
        roots.push(SkillRoot {
            path: repo_dir.join(".agents/skills"),
            scope: SkillScope::Repo,
            is_tool_root: false,
            is_plugin_root: false,
        });
    }

    if let Some(project_root) = &config.project_root {
        roots.push(SkillRoot {
            path: project_root.join(".agents/plugins"),
            scope: SkillScope::Repo,
            is_tool_root: false,
            is_plugin_root: true,
        });
        roots.push(SkillRoot {
            path: project_root.join("tools"),
            scope: SkillScope::Repo,
            is_tool_root: true,
            is_plugin_root: false,
        });
        roots.push(SkillRoot {
            path: project_root.join("vendor/tools"),
            scope: SkillScope::Repo,
            is_tool_root: true,
            is_plugin_root: false,
        });
    }

    if let Some(home) = home_dir {
        roots.push(SkillRoot {
            path: home.join(".agents/plugins"),
            scope: SkillScope::User,
            is_tool_root: false,
            is_plugin_root: true,
        });
        roots.push(SkillRoot {
            path: home.join(".agents/skills"),
            scope: SkillScope::User,
            is_tool_root: false,
            is_plugin_root: false,
        });

        if let Ok(paths) = VtCodePaths::resolve()
            && dirs::home_dir().as_deref() == Some(home)
        {
            roots.push(SkillRoot {
                path: paths.data_dir().join("skills"),
                scope: SkillScope::User,
                is_tool_root: false,
                is_plugin_root: false,
            });
            roots.push(SkillRoot {
                path: paths.legacy_dir().join("skills"),
                scope: SkillScope::User,
                is_tool_root: false,
                is_plugin_root: false,
            });
            roots.push(SkillRoot {
                path: paths.plugins_dir(),
                scope: SkillScope::User,
                is_tool_root: false,
                is_plugin_root: true,
            });
            roots.push(SkillRoot {
                path: paths.legacy_dir().join("plugins"),
                scope: SkillScope::User,
                is_tool_root: false,
                is_plugin_root: true,
            });
        }
    }

    #[cfg(unix)]
    roots.push(SkillRoot {
        path: PathBuf::from("/etc/codex/skills"),
        scope: SkillScope::Admin,
        is_tool_root: false,
        is_plugin_root: false,
    });

    if config.include_bundled_system_skills {
        roots.push(SkillRoot {
            path: system_cache_root_dir(&config.codex_home),
            scope: SkillScope::System,
            is_tool_root: false,
            is_plugin_root: false,
        });
    }

    roots
}

fn repo_skill_search_dirs(config: &SkillLoaderConfig) -> Vec<PathBuf> {
    let stop = config.project_root.clone().unwrap_or_else(|| config.cwd.clone());
    let mut dirs = Vec::new();
    let mut current = config.cwd.clone();

    loop {
        dirs.push(current.clone());
        if current == stop {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }

    dirs
}

pub(crate) fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn discover_skills_under_root(root: &SkillRoot, outcome: &mut SkillLoadOutcome) {
    walk_skills_dir(root, outcome, WalkMode::Full)
}

fn discover_metadata_under_root(root: &SkillRoot, outcome: &mut SkillLoadOutcome) {
    walk_skills_dir(root, outcome, WalkMode::Metadata)
}

#[derive(Clone, Copy)]
enum WalkMode {
    Full,
    Metadata,
}

fn walk_skills_dir(root: &SkillRoot, outcome: &mut SkillLoadOutcome, mode: WalkMode) {
    let Ok(root_path) = dunce::canonicalize(&root.path) else {
        return;
    };

    if !root_path.is_dir() {
        return;
    }

    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(root_path, 0)]);
    let mut dirs_visited: usize = 0;
    while let Some((dir, depth)) = queue.pop_front() {
        dirs_visited += 1;
        if dirs_visited > MAX_DISCOVERY_DIRS {
            warn!(
                "skill discovery dir budget ({MAX_DISCOVERY_DIRS}) exhausted under {}; some skills may be missing",
                root.path.display()
            );
            return;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                if matches!(mode, WalkMode::Full) {
                    error!("failed to read skills dir {}: {e:#}", dir.display());
                } else {
                    tracing::debug!("failed to read skills dir {}: {e:#}", dir.display());
                }
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = match path.file_name().and_then(|f| f.to_str()) {
                Some(name) => name,
                None => continue,
            };

            if file_name.starts_with('.') || discovery_dir_skipped(file_name) {
                continue;
            }

            if path.is_dir() {
                // Plugin-consumed directories are units: a successful plugin
                // load ends handling here so the walk never descends into
                // plugin subdirectories looking for more skills.
                if root.is_tool_root
                    && let Ok(Some(tool_meta)) = try_load_tool_from_dir(&path, root.scope)
                {
                    outcome.skills.push(tool_meta);
                }

                if root.is_plugin_root
                    && let Ok(Some(plugin_meta)) = try_load_plugin_from_dir(&path, root.scope)
                {
                    outcome.skills.push(plugin_meta);
                    continue;
                }

                if try_ingest_plugin_skills(&path, root, outcome) {
                    continue;
                }

                if depth < MAX_DISCOVERY_DEPTH {
                    queue.push_back((path.clone(), depth + 1));
                }
            }

            if file_name == "SKILL.md" {
                handle_skill_md(path.clone(), root, outcome, mode);
            } else if matches!(mode, WalkMode::Full) && root.is_tool_root && is_executable_file(&path) {
                // Standalone executable tools are recognized in the full walk
                // but ignored in the lightweight metadata walk.
            }
        }
    }
}

fn handle_skill_md(path: PathBuf, root: &SkillRoot, outcome: &mut SkillLoadOutcome, mode: WalkMode) {
    let skill_dir = match path.parent() {
        Some(dir) => dir,
        None => return,
    };

    match mode {
        WalkMode::Full => match crate::skills::manifest::parse_skill_file(skill_dir) {
            Ok((manifest, _)) => {
                outcome.skills.push(SkillMetadata {
                    name: manifest.name.clone(),
                    description: manifest.description.clone(),
                    short_description: None,
                    path,
                    scope: root.scope,
                    manifest: Some(manifest.into()),
                });
            }
            Err(err) => {
                if root.scope != SkillScope::System {
                    outcome.errors.push(SkillErrorInfo { path, message: err.to_string() });
                }
            }
        },
        WalkMode::Metadata => match fs::read_to_string(&path).with_context(|| format!("reading {}", path.display())) {
            Ok(contents) => match crate::skills::manifest::parse_skill_content(&contents) {
                Ok((manifest, _)) => {
                    outcome.skills.push(SkillMetadata {
                        name: manifest.name.clone(),
                        description: manifest.description.clone(),
                        short_description: None,
                        path,
                        scope: root.scope,
                        manifest: Some(manifest.into()),
                    });
                }
                Err(err) => {
                    if root.scope != SkillScope::System {
                        outcome.errors.push(SkillErrorInfo { path, message: err.to_string() });
                    }
                }
            },
            Err(err) => {
                if root.scope != SkillScope::System {
                    outcome.errors.push(SkillErrorInfo { path, message: err.to_string() });
                }
            }
        },
    }
}

fn try_load_tool_from_dir(path: &Path, scope: SkillScope) -> Result<Option<SkillMetadata>> {
    // Check if it's a CLI tool directory (has tool.json or is executable inside)
    // Simplified: check for tool.json
    let tool_bridge = if path.join("tool.json").exists() {
        CliToolBridge::from_directory(path)?
    } else {
        // Heuristic: check for executable with same name as dir?
        // This is complex to reproduce exactly "discovery.rs" logic without code dupe.
        // I'll be conservative and require tool.json OR evident executable.
        match CliToolBridge::from_directory(path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        }
    };

    tool_config_to_metadata(&tool_bridge.config, scope).map(Some)
}

/// Try to load a portable Agent Plugin from `path`. Returns `true` if a valid
/// plugin was loaded and its skills were ingested into `outcome`.
fn try_ingest_plugin_skills(path: &Path, root: &SkillRoot, outcome: &mut SkillLoadOutcome) -> bool {
    if !root.is_plugin_root {
        return false;
    }
    let Ok(loaded) = LoadedPlugin::load_from_dir(path) else {
        return false;
    };
    for skill in &loaded.skills {
        outcome.skills.push(SkillMetadata {
            name: skill.name.clone(),
            description: skill.manifest.description.clone(),
            short_description: None,
            path: skill.skill_md_path.clone(),
            scope: root.scope,
            manifest: Some(Box::new(skill.manifest.clone())),
        });
    }
    true
}

fn tool_config_to_metadata(config: &CliToolConfig, scope: SkillScope) -> Result<SkillMetadata> {
    Ok(SkillMetadata {
        name: config.name.clone(),
        description: config.description.clone(),
        short_description: None,
        path: config.executable_path.clone(), // Path to executable is the "path" of the skill?
        // Or path to directory? Reference uses SKILL.md path.
        // Here we use executable path or tool directory.
        scope,
        manifest: None, // CLI tools don't have a manifest in the same sense, or we could synthesize one
    })
}

fn try_load_plugin_from_dir(path: &Path, scope: SkillScope) -> Result<Option<SkillMetadata>> {
    // Check if it's a native plugin directory (has plugin.json)
    let plugin_json_path = path.join("plugin.json");
    if !plugin_json_path.exists() {
        return Ok(None);
    }

    // Read and parse plugin metadata
    let plugin_json_content = fs::read_to_string(&plugin_json_path).context("Failed to read plugin.json")?;

    let plugin_metadata: crate::skills::native_plugin::PluginMetadata =
        serde_json::from_str(&plugin_json_content).context("Invalid plugin.json format")?;

    // Validate that the plugin has a corresponding dynamic library
    let lib_name = crate::skills::native_plugin::PluginLoader::new().library_filename(&plugin_metadata.name);

    if !path.join(&lib_name).exists() {
        // Try alternative library names
        let alternatives = [
            format!("lib{}.dylib", plugin_metadata.name),
            format!("{}.dylib", plugin_metadata.name),
            format!("lib{}.so", plugin_metadata.name),
            format!("{}.so", plugin_metadata.name),
            format!("{}.dll", plugin_metadata.name),
        ];

        let has_lib = alternatives.iter().any(|alt| path.join(alt).exists());
        if !has_lib {
            return Ok(None); // No library found, skip this plugin
        }
    }

    Ok(Some(SkillMetadata {
        name: plugin_metadata.name.clone(),
        description: plugin_metadata.description.clone(),
        short_description: None,
        path: path.to_path_buf(),
        scope,
        manifest: None, // Native plugins don't have SKILL.md manifest
    }))
}

pub fn load_skill_resources(skill_path: &Path) -> Result<Vec<crate::skills::types::SkillResource>> {
    let mut resources = Vec::new();
    let resource_dir = skill_path.join("scripts");

    if resource_dir.exists() {
        for entry in fs::read_dir(&resource_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let rel_path = path
                    .strip_prefix(skill_path)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let resource_type = match path.extension().and_then(|e| e.to_str()) {
                    Some("py") | Some("sh") | Some("bash") => crate::skills::types::ResourceType::Script,
                    Some("md") => crate::skills::types::ResourceType::Markdown,
                    Some("json") | Some("yaml") | Some("yml") => crate::skills::types::ResourceType::Reference,
                    _ => crate::skills::types::ResourceType::Other(format!("{:?}", path.extension())),
                };

                resources.push(crate::skills::types::SkillResource { path: rel_path, resource_type, content: None });
            }
        }
    }

    // Check for references/ directory
    let references_dir = skill_path.join("references");
    if references_dir.exists() {
        for entry in fs::read_dir(&references_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let rel_path = path
                    .strip_prefix(skill_path)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let resource_type = match path.extension().and_then(|e| e.to_str()) {
                    Some("md") => crate::skills::types::ResourceType::Reference,
                    Some("json") | Some("yaml") | Some("yml") | Some("txt") | Some("csv") => {
                        crate::skills::types::ResourceType::Reference
                    }
                    _ => crate::skills::types::ResourceType::Other(format!("{:?}", path.extension())),
                };

                resources.push(crate::skills::types::SkillResource { path: rel_path, resource_type, content: None });
            }
        }
    }

    // Check for assets/ directory
    let assets_dir = skill_path.join("assets");
    if assets_dir.exists() {
        for entry in fs::read_dir(&assets_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                let rel_path = path
                    .strip_prefix(skill_path)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();

                let resource_type = match path.extension().and_then(|e| e.to_str()) {
                    Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("svg") => {
                        crate::skills::types::ResourceType::Asset
                    }
                    Some("json") | Some("yaml") | Some("yml") | Some("txt") | Some("csv") => {
                        crate::skills::types::ResourceType::Asset
                    }
                    _ => crate::skills::types::ResourceType::Asset,
                };

                resources.push(crate::skills::types::SkillResource { path: rel_path, resource_type, content: None });
            }
        }
    }

    Ok(resources)
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = path.metadata() {
            return meta.permissions().mode() & 0o111 != 0;
        }
    }
    #[cfg(windows)]
    {
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
            return matches!(ext.to_lowercase().as_str(), "exe" | "bat" | "cmd");
        }
    }
    false
}

/// Enhanced skill variant for unified handling
pub enum EnhancedSkill {
    /// Traditional instruction-based skill
    Traditional(Box<Skill>),
    /// CLI-based tool skill
    CliTool(Box<CliToolBridge>),
    /// Built-in VT Code command skill
    BuiltInCommand(Box<BuiltInCommandSkill>),
    /// Native code plugin skill reserved for explicitly gated integrations
    NativePlugin(Box<dyn crate::skills::native_plugin::NativePluginTrait>),
}

impl std::fmt::Debug for EnhancedSkill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Traditional(skill) => f.debug_tuple("Traditional").field(skill).finish(),
            Self::CliTool(tool) => f.debug_tuple("CliTool").field(tool).finish(),
            Self::BuiltInCommand(skill) => f.debug_tuple("BuiltInCommand").field(skill).finish(),
            Self::NativePlugin(plugin) => f.debug_tuple("NativePlugin").field(plugin).finish(),
        }
    }
}

/// High-level loader that provides discovery and validation features
pub struct EnhancedSkillLoader {
    workspace_root: PathBuf,
    codex_home: PathBuf,
    discovery: SkillDiscovery,
}

fn default_codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub(crate) fn discovery_config_for_codex_home(workspace_root: &Path, codex_home: &Path) -> DiscoveryConfig {
    let home_dir = dirs::home_dir();
    let loader_config = SkillLoaderConfig {
        codex_home: codex_home.to_path_buf(),
        cwd: workspace_root.to_path_buf(),
        project_root: find_git_root(workspace_root),
        include_bundled_system_skills: true,
    };
    let roots = skill_roots_with_home_dir(&loader_config, home_dir.as_deref());

    DiscoveryConfig {
        skill_paths: roots
            .iter()
            .filter(|root| !root.is_tool_root && !root.is_plugin_root)
            .map(|root| root.path.clone())
            .collect(),
        tool_paths: roots
            .iter()
            .filter(|root| root.is_tool_root)
            .map(|root| root.path.clone())
            .collect(),
        ..Default::default()
    }
}

impl EnhancedSkillLoader {
    /// Create a new enhanced loader for workspace
    pub fn new(workspace_root: PathBuf) -> Self {
        let codex_home = default_codex_home();
        let discovery = SkillDiscovery::with_config(discovery_config_for_codex_home(&workspace_root, &codex_home));
        Self { workspace_root, codex_home, discovery }
    }

    /// Create a loader pinned to a specific VT Code home directory.
    pub fn with_codex_home(workspace_root: PathBuf, codex_home: PathBuf) -> Self {
        let discovery = SkillDiscovery::with_config(discovery_config_for_codex_home(&workspace_root, &codex_home));
        Self { workspace_root, codex_home, discovery }
    }

    fn ensure_system_skills_installed(&self) {
        if let Err(err) = install_system_skills(&self.codex_home) {
            tracing::warn!("enhanced skill loader failed to install bundled system skills: {err}");
        }
    }

    /// Discover all available skills and tools
    pub async fn discover_all_skills(&mut self) -> Result<DiscoveryResult> {
        self.ensure_system_skills_installed();
        let mut result = self.discovery.discover_all(&self.workspace_root).await?;
        merge_built_in_command_skill_contexts(&mut result.skills);
        Ok(result)
    }

    /// Get a specific skill by name
    pub async fn get_skill(&mut self, name: &str) -> Result<EnhancedSkill> {
        self.ensure_system_skills_installed();
        let result = self.discovery.discover_all(&self.workspace_root).await?;

        // Try traditional skills first. Command skills (`cmd-*` with a bundled
        // `.system` contract) prefer the System copy so a stale workspace
        // shadow cannot break `/review` free-form + fix-mode behavior.
        if let Some(skill_ctx) = select_traditional_skill_ctx(&result.skills, name, &self.workspace_root) {
            let path = skill_ctx.path();
            let (manifest, instructions) = crate::skills::manifest::parse_skill_file(path)?;
            let skill = Skill::with_scope(
                manifest,
                path.clone(),
                infer_scope_from_skill_path(path, &self.workspace_root),
                instructions,
            )?;
            return Ok(EnhancedSkill::Traditional(Box::new(skill)));
        }

        // Try CLI tools
        for tool_config in &result.tools {
            if tool_config.name == name {
                let bridge = CliToolBridge::new(tool_config.clone())?;
                return Ok(EnhancedSkill::CliTool(Box::new(bridge)));
            }
        }

        if let Some(skill) = built_in_command_skill(name) {
            return Ok(EnhancedSkill::BuiltInCommand(Box::new(skill)));
        }

        // Native plugins are intentionally not loaded by the generic skill
        // lookup. Opening a dynamic library executes arbitrary native code
        // before this loader can inspect its metadata, so native loading must
        // remain an explicit, separately-gated operation.
        Err(skill_ops::skill_not_found_error(name))
    }

    /// Generate a comprehensive container validation report
    pub async fn generate_validation_report(&mut self) -> Result<ContainerValidationReport> {
        let result = self.discovery.discover_all(&self.workspace_root).await?;
        let mut report = ContainerValidationReport::new();
        let validator = ContainerSkillsValidator::new();

        for skill_ctx in &result.skills {
            match self.load_full_skill_from_ctx(skill_ctx) {
                Ok(skill) => {
                    let analysis = validator.analyze_skill(&skill);
                    report.add_skill_analysis(skill.name().to_string(), analysis);
                }
                Err(e) => {
                    report.add_incompatible_skill(
                        skill_ctx.manifest().name.clone(),
                        skill_ctx.manifest().description.clone(),
                        format!("Load error: {e}"),
                    );
                }
            }
        }

        report.finalize();
        Ok(report)
    }

    /// Check container requirements for a skill
    pub fn check_container_requirements(&self, skill: &Skill) -> ContainerValidationResult {
        let validator = ContainerSkillsValidator::new();
        validator.analyze_skill(skill)
    }

    fn load_full_skill_from_ctx(&self, ctx: &SkillContext) -> Result<Skill> {
        let path = ctx.path();
        let (manifest, instructions) = crate::skills::manifest::parse_skill_file(path)?;
        Skill::with_scope(manifest, path.clone(), infer_scope_from_skill_path(path, &self.workspace_root), instructions)
    }
}

fn infer_scope_from_skill_path(path: &Path, workspace_root: &Path) -> SkillScope {
    if path.starts_with(Path::new("/etc/codex/skills")) {
        return SkillScope::Admin;
    }
    if path.starts_with(system_cache_root_dir(&default_codex_home())) {
        return SkillScope::System;
    }
    if let Some(home) = dirs::home_dir()
        && path.starts_with(home.join(".agents/skills"))
    {
        return SkillScope::User;
    }
    if path.starts_with(workspace_root) {
        return SkillScope::Repo;
    }

    // Fallback for non-canonicalized paths: walk the components looking for a
    // `.agents/skills` ancestor. Uses proper component matching rather than
    // string-contains so a directory named `skills-backup` cannot trigger a
    // false positive.
    if has_agents_skills_ancestor(path) {
        return SkillScope::Repo;
    }

    SkillScope::User
}

fn has_agents_skills_ancestor(path: &Path) -> bool {
    let mut ancestors = path.parent().map(Path::new).into_iter().peekable();
    while let Some(current) = ancestors.next() {
        let is_skills = current.file_name().and_then(|f| f.to_str()) == Some("skills");
        let parent_is_agents = ancestors.peek().and_then(|p| p.file_name().and_then(|f| f.to_str())) == Some(".agents");
        if is_skills && parent_is_agents {
            return true;
        }
    }
    false
}

fn is_bundled_command_skill(name: &str) -> bool {
    find_command_skill_by_skill_name(name)
        .is_some_and(|spec| matches!(spec.backend, CommandSkillBackend::TraditionalSkill { .. }))
}

fn is_system_skill_path(path: &Path) -> bool {
    let mut components = path.components().peekable();
    while let Some(component) = components.next() {
        let is_skills = component.as_os_str() == "skills";
        let next_is_system = components.peek().is_some_and(|next| next.as_os_str() == ".system");
        if is_skills && next_is_system {
            return true;
        }
    }
    false
}

fn select_traditional_skill_ctx<'a>(
    skills: &'a [SkillContext],
    name: &str,
    workspace_root: &Path,
) -> Option<&'a SkillContext> {
    let mut first_match: Option<&'a SkillContext> = None;
    let mut system_match: Option<&'a SkillContext> = None;
    for skill_ctx in skills {
        if skill_ctx.manifest().name != name {
            continue;
        }
        if first_match.is_none() {
            first_match = Some(skill_ctx);
        }
        // Shape-based system detection covers custom `CODEX_HOME` values where
        // `infer_scope_from_skill_path` falls back to `User`. Exclude `Repo`
        // scope so a workspace-nested `skills/.system` path cannot spoof it.
        let scope = infer_scope_from_skill_path(skill_ctx.path(), workspace_root);
        if system_match.is_none()
            && (scope == SkillScope::System || (scope != SkillScope::Repo && is_system_skill_path(skill_ctx.path())))
        {
            system_match = Some(skill_ctx);
        }
        if first_match.is_some() && system_match.is_some() {
            break;
        }
    }
    if is_bundled_command_skill(name) {
        system_match.or(first_match)
    } else {
        first_match
    }
}

/// Test helper for hermetic skill loading that does not pick up user skills.
/// Use this in tests to avoid failures when ~/.agents/skills contains skills.
#[cfg(test)]
pub fn load_skills_hermetic(config: &SkillLoaderConfig) -> SkillLoadOutcome {
    load_skills_with_home_dir(config, None)
}

/// Test helper for hermetic lightweight skill discovery.
#[cfg(test)]
pub fn discover_skill_metadata_lightweight_hermetic(config: &SkillLoaderConfig) -> SkillLoadOutcome {
    discover_skill_metadata_lightweight_with_home_dir(config, None)
}

#[cfg(test)]
mod tests;

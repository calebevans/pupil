use std::collections::{HashMap, HashSet, VecDeque};

use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::agent_config;
use crate::container::{ContainerId, ContainerRuntime, RunOptions};
use crate::error::CliError;

// ── Manifest schema ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub namespace: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dims: u32,
    pub sources: HashMap<String, SourceEntry>,
    #[serde(default)]
    pub builds: Vec<BuildEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEntry {
    pub content_hash: String,
    #[serde(default)]
    pub prompt_hash: String,
    pub memory_ids: Vec<String>,
    pub last_learned: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    pub last_checked: Option<String>,
    pub last_changed: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_hash: Option<String>,
    pub check_count: u64,
    pub change_count: u64,
    pub consecutive_errors: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEntry {
    pub timestamp: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub sources_learned: u32,
    pub sources_skipped: u32,
    pub memories_created: u32,
}

// ── Memory data model ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_text: Option<String>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub strength: f64,
    pub phase: String,
    pub created_at: String,
    pub accessed_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    pub namespace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub score: f64,
    #[serde(flatten)]
    pub memory: Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarResult {
    pub similarity: f64,
    #[serde(flatten)]
    pub memory: Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    pub from_id: String,
    pub to_id: String,
    pub relation_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

// ── Output types ───────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ListOutput {
    pub agent: String,
    pub total: usize,
    pub sources: usize,
    pub memories: Vec<Memory>,
}

#[derive(Debug, Serialize)]
pub struct ShowOutput {
    pub memory: Memory,
    pub relationships: Vec<Relationship>,
}

#[derive(Debug, Serialize)]
pub struct SearchOutput {
    pub query: String,
    pub total: usize,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Serialize)]
pub struct StatsOutput {
    pub agent: String,
    pub total_memories: usize,
    pub namespace: String,
    pub image: String,
    pub built: Option<String>,
    pub phase_breakdown: PhaseBreakdown,
    pub by_source: Vec<SourceStats>,
    pub by_type_tag: Vec<TagStats>,
    pub top_entities: Vec<EntityStats>,
}

#[derive(Debug, Serialize)]
pub struct PhaseBreakdown {
    pub full: usize,
    pub summary: usize,
    pub ghost: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceStats {
    pub source: String,
    pub count: usize,
    pub share: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TagStats {
    pub tag: String,
    pub count: usize,
    pub share: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityStats {
    pub entity: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct QualityOutput {
    pub agent: String,
    pub near_duplicates: Vec<DuplicatePair>,
    pub orphaned_memories: Vec<OrphanedMemory>,
    pub missing_metadata: Vec<MetadataIssue>,
    pub weak_memories: Vec<WeakMemory>,
    pub superseded_chains: Vec<SupersededChain>,
    pub empty_sources: Vec<String>,
    pub warnings: usize,
    pub errors: usize,
}

#[derive(Debug, Serialize)]
pub struct DuplicatePair {
    pub id_a: String,
    pub id_b: String,
    pub similarity: f64,
    pub summary_a: String,
    pub summary_b: String,
}

#[derive(Debug, Serialize)]
pub struct OrphanedMemory {
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct MetadataIssue {
    pub id: String,
    pub summary: String,
    pub missing: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WeakMemory {
    pub id: String,
    pub summary: String,
    pub strength: f64,
}

#[derive(Debug, Serialize)]
pub struct SupersededChain {
    pub old_id: String,
    pub old_summary: String,
    pub new_id: String,
    pub new_summary: String,
}

#[derive(Debug, Serialize)]
pub struct GraphOutput {
    pub agent: String,
    pub total_memories: usize,
    pub total_relationships: usize,
    pub components: usize,
    pub isolated: Vec<IsolatedMemory>,
    pub edge_type_breakdown: Vec<EdgeTypeCount>,
    pub largest_component_size: usize,
    pub largest_component_center: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IsolatedMemory {
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgeTypeCount {
    pub edge_type: String,
    pub count: usize,
    pub share: f64,
}

#[derive(Debug, Serialize)]
pub struct DiffOutput {
    pub agent: String,
    pub tag_a: String,
    pub tag_b: String,
    pub added: Vec<DiffMemory>,
    pub removed: Vec<DiffMemory>,
    pub unchanged_count: usize,
    pub changed_sources: Vec<ChangedSource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffMemory {
    pub id: String,
    pub source: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedSource {
    pub source: String,
    pub added: usize,
    pub removed: usize,
    pub note: String,
}

// ── CLI argument definitions ───────────────────────────────────────

#[derive(Args, Debug)]
pub struct InspectArgs {
    pub name: Option<String>,

    #[command(subcommand)]
    pub action: Option<InspectAction>,

    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true)]
    pub source: Option<String>,

    #[arg(long, global = true)]
    pub namespace: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum InspectAction {
    List(ListActionArgs),
    Show(ShowActionArgs),
    Search(SearchActionArgs),
    Stats(StatsActionArgs),
    Quality(QualityActionArgs),
    Graph(GraphActionArgs),
    Diff(DiffActionArgs),
}

#[derive(Debug, Args)]
pub struct ListActionArgs {
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long, default_value = "source")]
    pub sort: ListSort,
    #[arg(long)]
    pub limit: Option<usize>,
}

impl Default for ListActionArgs {
    fn default() -> Self {
        Self {
            tag: None,
            sort: ListSort::Source,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum ListSort {
    Source,
    Strength,
    Created,
    Id,
}

#[derive(Debug, Args)]
pub struct ShowActionArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct SearchActionArgs {
    pub query: String,
    #[arg(long, default_value = "10")]
    pub limit: usize,
    #[arg(long)]
    pub min_score: Option<f64>,
    #[arg(long)]
    pub min_strength: Option<f64>,
}

#[derive(Debug, Args)]
pub struct StatsActionArgs {}

#[derive(Debug, Args)]
pub struct QualityActionArgs {
    #[arg(long, default_value = "0.90")]
    pub duplicate_threshold: f64,
    #[arg(long, default_value = "0.50")]
    pub weak_threshold: f64,
    #[arg(long)]
    pub fix: bool,
}

#[derive(Debug, Args)]
pub struct GraphActionArgs {
    #[arg(long)]
    pub memory: Option<String>,
    #[arg(long, default_value = "1")]
    pub depth: u32,
    #[arg(long)]
    pub dot: bool,
}

#[derive(Debug, Args)]
pub struct DiffActionArgs {
    pub tag_a: String,
    pub tag_b: String,
}

// ── InspectContext ──────────────────────────────────────────────────

struct InspectContext {
    runtime: Box<dyn ContainerRuntime>,
    container_id: ContainerId,
    agent_name: String,
    image_ref: String,
    manifest: Manifest,
    namespace: String,
}

impl InspectContext {
    async fn new(
        name: Option<&str>,
        namespace_override: Option<&str>,
    ) -> Result<Self, CliError> {
        let agent_name = resolve_agent_name(name)?;

        let runtime = crate::container::detect()
            .map_err(|_| CliError::ContainerRuntimeNotFound)?;

        let image_ref = agent_config::image_ref(&agent_name, None);

        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..6];
        let run_opts = RunOptions {
            read_only: true,
            network: Some("none".to_string()),
            name: Some(format!("pupil-inspect-{}-{}", agent_name, suffix)),
            detach: true,
            remove_on_exit: true,
            entrypoint: Some("sleep".to_string()),
            command: vec!["infinity".to_string()],
            tmpfs: vec!["/tmp".to_string()],
            ..Default::default()
        };

        let container_id = runtime
            .run(&image_ref, &run_opts)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to start inspection container: {e}"),
            })?;

        let manifest_output = match runtime
            .exec(&container_id, &["cat", "/data/.pupil-manifest.json"], &[])
            .await
        {
            Ok(output) => output,
            Err(_) => {
                let _ = runtime.rm(&container_id, true).await;
                return Err(CliError::ContainerRuntimeError {
                    message: "No learning data found. Run `pupil build` first.".to_string(),
                });
            }
        };

        let manifest: Manifest = serde_json::from_str(&manifest_output.stdout)
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to parse manifest: {e}"),
            })?;

        let ns = namespace_override
            .map(String::from)
            .unwrap_or_else(|| manifest.namespace.clone());

        Ok(Self {
            agent_name,
            namespace: ns,
            runtime,
            container_id,
            manifest,
            image_ref,
        })
    }

    async fn recalld(&self, args: &[&str]) -> Result<String, CliError> {
        let mut cmd = vec!["recalld"];
        cmd.extend_from_slice(args);
        let output = self
            .runtime
            .exec(&self.container_id, &cmd, &[])
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("recalld {} failed: {e}", args.join(" ")),
            })?;
        if output.exit_code != 0 {
            return Err(CliError::ContainerRuntimeError {
                message: format!(
                    "recalld {} failed (exit {}): {}",
                    args.join(" "),
                    output.exit_code,
                    output.stderr
                ),
            });
        }
        Ok(output.stdout)
    }

    async fn recalld_json<T: serde::de::DeserializeOwned>(
        &self,
        args: &[&str],
    ) -> Result<T, CliError> {
        let output = self.recalld(args).await?;
        serde_json::from_str(&output).map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to parse recalld JSON output: {e}"),
        })
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn agent_name(&self) -> &str {
        &self.agent_name
    }

    fn image_ref(&self) -> &str {
        &self.image_ref
    }

    fn resolve_id(&self, short_id: &str) -> Result<String, CliError> {
        let normalized = short_id.replace('-', "").to_lowercase();
        if normalized.len() < 4 {
            return Err(CliError::ContainerRuntimeError {
                message: format!(
                    "Memory ID '{}' is too short. Use at least 4 hex characters.",
                    short_id
                ),
            });
        }

        let mut matches = Vec::new();
        for source_entry in self.manifest.sources.values() {
            for mid in &source_entry.memory_ids {
                let mid_normalized = mid.replace('-', "").to_lowercase();
                if mid_normalized.starts_with(&normalized) {
                    matches.push(mid.clone());
                }
            }
        }

        match matches.len() {
            0 => Err(CliError::ContainerRuntimeError {
                message: format!(
                    "Memory '{}' not found. Run `pupil inspect list` to see all memory IDs.",
                    short_id
                ),
            }),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => {
                let display: Vec<String> =
                    matches.iter().take(5).map(|m| format_short_id(m)).collect();
                Err(CliError::ContainerRuntimeError {
                    message: format!(
                        "Ambiguous memory ID '{}' matches {} memories: {}",
                        short_id,
                        n,
                        display.join(", ")
                    ),
                })
            }
        }
    }

    async fn cleanup(&self) -> Result<(), CliError> {
        self.runtime
            .rm(&self.container_id, true)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to remove inspection container: {e}"),
            })
    }
}

impl Drop for InspectContext {
    fn drop(&mut self) {
        tracing::debug!(
            container = %self.container_id,
            "InspectContext dropped; container should auto-remove via --rm"
        );
    }
}

// ── Helper functions ───────────────────────────────────────────────

fn resolve_agent_name(name: Option<&str>) -> Result<String, CliError> {
    match name {
        Some(n) => Ok(n.to_string()),
        None => {
            let cwd = std::env::current_dir()?;
            let config = crate::agent_config::AgentConfig::load(&cwd)?;
            Ok(config.name)
        }
    }
}

pub fn format_short_id(uuid: &str) -> String {
    let hex: String = uuid.chars().filter(|c| *c != '-').collect();
    if hex.len() < 6 {
        return hex;
    }
    format!("{}..{}", &hex[..4], &hex[hex.len() - 2..])
}

pub fn extract_source(tags: &[String]) -> String {
    for tag in tags {
        if let Some(source) = tag.strip_prefix("source/") {
            return source.to_string();
        }
    }
    "unknown".to_string()
}

pub fn truncate(s: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if s.len() <= max_width {
        return s.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut truncated = String::new();
    for ch in s.chars() {
        if truncated.len() + ch.len_utf8() > max_width - 3 {
            break;
        }
        truncated.push(ch);
    }
    truncated.push_str("...");
    truncated
}

fn term_width() -> usize {
    console::Term::stdout()
        .size_checked()
        .map(|(_, w)| w as usize)
        .unwrap_or(80)
}

fn filter_by_source(memories: &[Memory], source_filter: Option<&str>) -> Vec<Memory> {
    match source_filter {
        None => memories.to_vec(),
        Some(filter) => memories
            .iter()
            .filter(|m| {
                m.tags.iter().any(|tag| {
                    tag.strip_prefix("source/")
                        .map(|s| s.contains(filter))
                        .unwrap_or(false)
                })
            })
            .cloned()
            .collect(),
    }
}

fn filter_by_tag(memories: &[Memory], tag_filter: Option<&str>) -> Vec<Memory> {
    match tag_filter {
        None => memories.to_vec(),
        Some(filter) => memories
            .iter()
            .filter(|m| m.tags.iter().any(|t| t == filter))
            .cloned()
            .collect(),
    }
}

fn relationship_arrow(relation_type: &str) -> &'static str {
    match relation_type {
        "parent" | "child" => "->",
        "associative" => "<>",
        "entity" => "~~",
        "supersedes" => "=>",
        "superseded_by" => "<=",
        "temporal" | "causal" | "contradicts" => "><",
        _ => "--",
    }
}

fn relationship_arrow_reverse(relation_type: &str) -> &'static str {
    match relation_type {
        "parent" => "<-",
        "child" => "<-",
        "associative" => "<>",
        "entity" => "~~",
        "supersedes" => "<=",
        "superseded_by" => "=>",
        "temporal" | "causal" | "contradicts" => "><",
        _ => "--",
    }
}

// ── Entry point ────────────────────────────────────────────────────

pub async fn execute(args: InspectArgs) -> Result<(), CliError> {
    let ctx = InspectContext::new(
        args.name.as_deref(),
        args.namespace.as_deref(),
    )
    .await?;

    let result = match args.action {
        None => {
            execute_list(&ctx, &ListActionArgs::default(), args.source.as_deref(), args.json).await
        }
        Some(InspectAction::List(list_args)) => {
            execute_list(&ctx, &list_args, args.source.as_deref(), args.json).await
        }
        Some(InspectAction::Show(show_args)) => {
            execute_show(&ctx, &show_args, args.json).await
        }
        Some(InspectAction::Search(search_args)) => {
            execute_search(&ctx, &search_args, args.source.as_deref(), args.json).await
        }
        Some(InspectAction::Stats(stats_args)) => {
            execute_stats(&ctx, &stats_args, args.source.as_deref(), args.json).await
        }
        Some(InspectAction::Quality(quality_args)) => {
            execute_quality(&ctx, &quality_args, args.json).await
        }
        Some(InspectAction::Graph(graph_args)) => {
            execute_graph(&ctx, &graph_args, args.json).await
        }
        Some(InspectAction::Diff(diff_args)) => {
            execute_diff(&ctx, &diff_args, args.source.as_deref(), args.json).await
        }
    };

    let _ = ctx.cleanup().await;
    result
}

// ── 1. list subcommand ─────────────────────────────────────────────

async fn execute_list(
    ctx: &InspectContext,
    args: &ListActionArgs,
    source_filter: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    let memories: Vec<Memory> = ctx
        .recalld_json(&["list", "--json", "--namespace", ctx.namespace()])
        .await?;

    let memories = filter_by_source(&memories, source_filter);
    let memories = filter_by_tag(&memories, args.tag.as_deref());

    let mut memories = memories;
    match args.sort {
        ListSort::Source => {
            memories.sort_by(|a, b| {
                let sa = extract_source(&a.tags);
                let sb = extract_source(&b.tags);
                sa.cmp(&sb).then(a.id.cmp(&b.id))
            });
        }
        ListSort::Strength => {
            memories.sort_by(|a, b| {
                b.strength
                    .partial_cmp(&a.strength)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            });
        }
        ListSort::Created => {
            memories.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }
        ListSort::Id => {
            memories.sort_by(|a, b| a.id.cmp(&b.id));
        }
    }

    if let Some(limit) = args.limit {
        memories.truncate(limit);
    }

    let source_count: usize = {
        let mut sources = HashSet::new();
        for m in &memories {
            sources.insert(extract_source(&m.tags));
        }
        sources.len()
    };

    if json {
        let output = ListOutput {
            agent: ctx.agent_name().to_string(),
            total: memories.len(),
            sources: source_count,
            memories,
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        if memories.is_empty() {
            match source_filter {
                Some(f) => println!("No memories found matching source '{f}'."),
                None => println!("No memories found."),
            }
            return Ok(());
        }

        let total = memories.len();
        println!(
            "Memories for {} ({} total, {} sources)\n",
            ctx.agent_name(),
            total,
            source_count
        );

        let term_w = term_width();
        let summary_width = term_w.saturating_sub(10 + 22 + 10 + 8 + 8);

        let green = console::Style::new().green();
        let yellow = console::Style::new().yellow();
        let red = console::Style::new().red();
        let dim = console::Style::new().dim();

        println!(
            "{:<10} {:<20} {:<width$} {:>10} {:<8}",
            "ID",
            "SOURCE",
            "SUMMARY",
            "STRENGTH",
            "PHASE",
            width = summary_width
        );

        for m in &memories {
            let id_str = format_short_id(&m.id);
            let source_str = truncate(&extract_source(&m.tags), 20);
            let summary_str = truncate(&m.summary, summary_width);

            let strength_colored = if m.strength >= 0.8 {
                green.apply_to(format!("{:.2}", m.strength))
            } else if m.strength >= 0.5 {
                yellow.apply_to(format!("{:.2}", m.strength))
            } else {
                red.apply_to(format!("{:.2}", m.strength))
            };

            let phase_colored = match m.phase.to_lowercase().as_str() {
                "full" => green.apply_to(&m.phase),
                "summary" => yellow.apply_to(&m.phase),
                _ => red.apply_to(&m.phase),
            };

            println!(
                "{:<10} {:<20} {:<width$} {:>10} {:<8}",
                dim.apply_to(&id_str),
                source_str,
                summary_str,
                strength_colored,
                phase_colored,
                width = summary_width
            );
        }
    }

    Ok(())
}

// ── 2. show subcommand ─────────────────────────────────────────────

async fn execute_show(
    ctx: &InspectContext,
    args: &ShowActionArgs,
    json: bool,
) -> Result<(), CliError> {
    let full_id = ctx.resolve_id(&args.id)?;

    let memory: Memory = ctx
        .recalld_json(&["inspect", &full_id, "--json", "--namespace", ctx.namespace()])
        .await?;

    let relationships: Vec<Relationship> = ctx
        .recalld_json(&[
            "relationships",
            &full_id,
            "--json",
            "--namespace",
            ctx.namespace(),
        ])
        .await
        .unwrap_or_default();

    if json {
        let output = ShowOutput {
            memory,
            relationships,
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        let green = console::Style::new().green();
        let yellow = console::Style::new().yellow();
        let red = console::Style::new().red();
        let dim = console::Style::new().dim();
        let bold = console::Style::new().bold();

        println!("{}", bold.apply_to(format!("Memory {}", full_id)));
        println!();

        println!("  {:<12}{}", bold.apply_to("Summary:"), memory.summary);
        println!();

        match &memory.full_text {
            Some(text) => {
                let lines: Vec<&str> = text.lines().collect();
                if let Some(first) = lines.first() {
                    println!("  {:<12}{}", bold.apply_to("Full Text:"), first);
                    let indent = "              ";
                    for line in lines.iter().skip(1) {
                        println!("{}{}", indent, line);
                    }
                }
            }
            None => {
                println!(
                    "  {:<12}{}",
                    bold.apply_to("Full Text:"),
                    dim.apply_to("(none)")
                );
            }
        }
        println!();

        println!(
            "  {:<12}{}",
            bold.apply_to("Source:"),
            extract_source(&memory.tags)
        );
        println!(
            "  {:<12}{}",
            bold.apply_to("Namespace:"),
            memory.namespace
        );

        let strength_str = format!("{:.2}", memory.strength);
        let strength_colored = if memory.strength >= 0.8 {
            green.apply_to(&strength_str)
        } else if memory.strength >= 0.5 {
            yellow.apply_to(&strength_str)
        } else {
            red.apply_to(&strength_str)
        };
        println!("  {:<12}{}", bold.apply_to("Strength:"), strength_colored);

        let phase_colored = match memory.phase.to_lowercase().as_str() {
            "full" => green.apply_to(&memory.phase),
            "summary" => yellow.apply_to(&memory.phase),
            _ => red.apply_to(&memory.phase),
        };
        println!("  {:<12}{}", bold.apply_to("Phase:"), phase_colored);
        println!(
            "  {:<12}{}",
            bold.apply_to("Created:"),
            memory.created_at
        );
        println!(
            "  {:<12}{}",
            bold.apply_to("Accessed:"),
            memory.accessed_at
        );
        println!();

        let entities_str = if memory.entities.is_empty() {
            dim.apply_to("(none)").to_string()
        } else {
            memory.entities.join(", ")
        };
        println!("  {:<12}{}", bold.apply_to("Entities:"), entities_str);

        let topics_str = if memory.topics.is_empty() {
            dim.apply_to("(none)").to_string()
        } else {
            memory.topics.join(", ")
        };
        println!("  {:<12}{}", bold.apply_to("Topics:"), topics_str);

        let tags_str = memory.tags.join(", ");
        println!("  {:<12}{}", bold.apply_to("Tags:"), tags_str);
        println!();

        println!("  {}:", bold.apply_to("Relationships"));
        if relationships.is_empty() {
            println!("    {}", dim.apply_to("(none)"));
        } else {
            for rel in &relationships {
                let (other_id, arrow) = if rel.from_id == full_id {
                    (&rel.to_id, relationship_arrow(&rel.relation_type))
                } else {
                    (&rel.from_id, relationship_arrow_reverse(&rel.relation_type))
                };

                let other_short = format_short_id(other_id);

                let other_summary = ctx
                    .recalld_json::<Memory>(&[
                        "inspect",
                        other_id,
                        "--json",
                        "--namespace",
                        ctx.namespace(),
                    ])
                    .await
                    .map(|m| truncate(&m.summary, 50))
                    .unwrap_or_else(|_| "(unavailable)".to_string());

                let label_part = match &rel.label {
                    Some(label) => format!("  (via: {})", label),
                    None => String::new(),
                };

                let rel_type_display = match rel.relation_type.as_str() {
                    "parent" => "(parent)",
                    "child" => "(child)",
                    "associative" => "(assoc.)",
                    "entity" => "(entity)",
                    "supersedes" => "(supersedes)",
                    "superseded_by" => "(superseded)",
                    "temporal" => "(temporal)",
                    "causal" => "(causal)",
                    "contradicts" => "(contradicts)",
                    other => other,
                };

                println!(
                    "    {} {}  {:<14} \"{}\"{}",
                    arrow,
                    dim.apply_to(&other_short),
                    dim.apply_to(rel_type_display),
                    other_summary,
                    dim.apply_to(&label_part),
                );
            }
        }
    }

    Ok(())
}

// ── 3. search subcommand ───────────────────────────────────────────

async fn execute_search(
    ctx: &InspectContext,
    args: &SearchActionArgs,
    source_filter: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    if args.query.is_empty() {
        println!("Search query cannot be empty.");
        return Ok(());
    }

    let limit_str = args.limit.to_string();

    let mut cmd_args = vec![
        "recall",
        &args.query,
        "--json",
        "--namespace",
        ctx.namespace(),
        "--limit",
        &limit_str,
    ];

    let min_strength_str;
    if let Some(min_strength) = args.min_strength {
        min_strength_str = min_strength.to_string();
        cmd_args.push("--min-strength");
        cmd_args.push(&min_strength_str);
    }

    let results: Vec<SearchResult> = match ctx.recalld_json(&cmd_args).await {
        Ok(r) => r,
        Err(e) => {
            let err_msg = format!("{e}");
            if err_msg.contains("embedding") || err_msg.contains("Embedding") {
                eprintln!(
                    "{}",
                    console::Style::new().yellow().apply_to(
                        "Warning: Semantic search unavailable. Falling back to full-text search. Results may be less relevant."
                    )
                );
                let fts_args = vec![
                    "search",
                    &args.query,
                    "--json",
                    "--namespace",
                    ctx.namespace(),
                    "--limit",
                    &limit_str,
                ];
                ctx.recalld_json(&fts_args).await?
            } else {
                return Err(e);
            }
        }
    };

    let results: Vec<SearchResult> = if let Some(filter) = source_filter {
        results
            .into_iter()
            .filter(|r| {
                r.memory.tags.iter().any(|tag| {
                    tag.strip_prefix("source/")
                        .map(|s| s.contains(filter))
                        .unwrap_or(false)
                })
            })
            .collect()
    } else {
        results
    };

    let results: Vec<SearchResult> = if let Some(min_score) = args.min_score {
        results
            .into_iter()
            .filter(|r| r.score >= min_score)
            .collect()
    } else {
        results
    };

    if json {
        let output = SearchOutput {
            query: args.query.clone(),
            total: results.len(),
            results,
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        if results.is_empty() {
            println!("No memories matched your query.");
            println!();
            let dim = console::Style::new().dim();
            println!(
                "{}",
                dim.apply_to(
                    "If the topic should be covered, check the curriculum or adjust the learning prompt."
                )
            );
            return Ok(());
        }

        println!(
            "Search: \"{}\" ({} results)\n",
            args.query,
            results.len()
        );

        let term_w = term_width();
        let summary_width = term_w.saturating_sub(6 + 10 + 20 + 6);

        let green = console::Style::new().green();
        let yellow = console::Style::new().yellow();
        let dim = console::Style::new().dim();

        println!(
            "{:<6} {:<10} {:<20} {}",
            "SCORE", "ID", "SOURCE", "SUMMARY"
        );

        for r in &results {
            let score_colored = if r.score >= 0.7 {
                green.apply_to(format!("{:.2}", r.score))
            } else if r.score >= 0.4 {
                yellow.apply_to(format!("{:.2}", r.score))
            } else {
                dim.apply_to(format!("{:.2}", r.score))
            };

            let id_str = format_short_id(&r.memory.id);
            let source_str = truncate(&extract_source(&r.memory.tags), 20);
            let summary_str = truncate(&r.memory.summary, summary_width);

            println!(
                "{:<6} {:<10} {:<20} {}",
                score_colored,
                dim.apply_to(&id_str),
                source_str,
                summary_str,
            );
        }
    }

    Ok(())
}

// ── 4. stats subcommand ────────────────────────────────────────────

async fn execute_stats(
    ctx: &InspectContext,
    _args: &StatsActionArgs,
    source_filter: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    let memories: Vec<Memory> = ctx
        .recalld_json(&["list", "--json", "--namespace", ctx.namespace()])
        .await?;

    let memories = filter_by_source(&memories, source_filter);

    if memories.is_empty() {
        println!("No memories found.");
        return Ok(());
    }

    let total = memories.len();

    let mut full_count = 0usize;
    let mut summary_count = 0usize;
    let mut ghost_count = 0usize;
    for m in &memories {
        match m.phase.to_lowercase().as_str() {
            "full" => full_count += 1,
            "summary" => summary_count += 1,
            _ => ghost_count += 1,
        }
    }

    let mut source_counts: HashMap<String, usize> = HashMap::new();
    for m in &memories {
        let source = extract_source(&m.tags);
        *source_counts.entry(source).or_insert(0) += 1;
    }
    let mut by_source: Vec<SourceStats> = source_counts
        .iter()
        .map(|(source, count)| SourceStats {
            source: source.clone(),
            count: *count,
            share: *count as f64 / total as f64 * 100.0,
        })
        .collect();
    by_source.sort_by(|a, b| b.count.cmp(&a.count));

    let mut type_tag_counts: HashMap<String, usize> = HashMap::new();
    for m in &memories {
        for tag in &m.tags {
            if tag.starts_with("type/") {
                *type_tag_counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut by_type_tag: Vec<TagStats> = type_tag_counts
        .iter()
        .map(|(tag, count)| TagStats {
            tag: tag.clone(),
            count: *count,
            share: *count as f64 / total as f64 * 100.0,
        })
        .collect();
    by_type_tag.sort_by(|a, b| b.count.cmp(&a.count));

    let mut entity_counts: HashMap<String, usize> = HashMap::new();
    for m in &memories {
        for entity in &m.entities {
            *entity_counts.entry(entity.clone()).or_insert(0) += 1;
        }
    }
    let mut top_entities: Vec<EntityStats> = entity_counts
        .into_iter()
        .map(|(entity, count)| EntityStats { entity, count })
        .collect();
    top_entities.sort_by(|a, b| b.count.cmp(&a.count));
    top_entities.truncate(20);

    let built = ctx
        .manifest()
        .builds
        .last()
        .map(|b| b.timestamp.clone());

    let stats = StatsOutput {
        agent: ctx.agent_name().to_string(),
        total_memories: total,
        namespace: ctx.namespace().to_string(),
        image: ctx.image_ref().to_string(),
        built: built.clone(),
        phase_breakdown: PhaseBreakdown {
            full: full_count,
            summary: summary_count,
            ghost: ghost_count,
        },
        by_source: by_source.clone(),
        by_type_tag: by_type_tag.clone(),
        top_entities: top_entities.clone(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&stats).unwrap());
    } else {
        let bold = console::Style::new().bold();

        let header = match source_filter {
            Some(f) => format!(
                "Knowledge Statistics for {} (source: {})",
                ctx.agent_name(),
                f
            ),
            None => format!("Knowledge Statistics for {}", ctx.agent_name()),
        };
        println!("{}\n", bold.apply_to(header));

        println!("  {:<20}{}", bold.apply_to("Total memories:"), total);
        println!(
            "  {:<20}{}",
            bold.apply_to("Namespace:"),
            ctx.namespace()
        );
        println!(
            "  {:<20}{}",
            bold.apply_to("Image:"),
            ctx.image_ref()
        );
        println!(
            "  {:<20}{}",
            bold.apply_to("Built:"),
            built.as_deref().unwrap_or("(unknown)")
        );
        println!();

        println!("  {}:", bold.apply_to("Phase breakdown"));
        let pct =
            |n: usize| -> String { format!("{:5.1}%", n as f64 / total as f64 * 100.0) };
        println!(
            "    {:<18}{:>4}  ({})",
            "Full:",
            full_count,
            pct(full_count)
        );
        println!(
            "    {:<18}{:>4}  ({})",
            "Summary:",
            summary_count,
            pct(summary_count)
        );
        println!(
            "    {:<18}{:>4}  ({})",
            "Ghost:",
            ghost_count,
            pct(ghost_count)
        );
        println!();

        println!("  {}:", bold.apply_to("By source"));
        let max_bar_width = 16;
        let max_count = by_source.first().map(|s| s.count).unwrap_or(1);
        println!(
            "    {:<26} {:>8}   {:>5}",
            "SOURCE", "MEMORIES", "SHARE"
        );
        for s in &by_source {
            let bar_len = ((s.count as f64 / max_count as f64) * max_bar_width as f64)
                .ceil()
                .max(1.0) as usize;
            let bar: String = "\u{2588}".repeat(bar_len);
            println!(
                "    {:<26} {:>8}   {:>5.1}%   {}",
                truncate(&s.source, 26),
                s.count,
                s.share,
                console::Style::new().green().apply_to(&bar),
            );
        }
        println!();

        if !by_type_tag.is_empty() {
            println!("  {}:", bold.apply_to("By type tag"));
            for t in &by_type_tag {
                println!(
                    "    {:<26}{:>4}  ({:>5.1}%)",
                    t.tag, t.count, t.share
                );
            }
            println!();
        }

        if !top_entities.is_empty() {
            println!(
                "  {}:",
                bold.apply_to("Top entities (by mention count)")
            );
            let entity_strs: Vec<String> = top_entities
                .iter()
                .map(|e| format!("{} ({})", e.entity, e.count))
                .collect();
            let line = entity_strs.join(", ");
            println!("    {}", line);
        }
    }

    Ok(())
}

// ── 5. quality subcommand ──────────────────────────────────────────

async fn execute_quality(
    ctx: &InspectContext,
    args: &QualityActionArgs,
    json: bool,
) -> Result<(), CliError> {
    let memories: Vec<Memory> = ctx
        .recalld_json(&["list", "--json", "--namespace", ctx.namespace()])
        .await?;

    let total = memories.len();

    let memory_id_set: HashSet<String> = memories.iter().map(|m| m.id.clone()).collect();

    let manifest_id_set: HashSet<String> = ctx
        .manifest()
        .sources
        .values()
        .flat_map(|s| s.memory_ids.iter().cloned())
        .collect();

    // 2a. Near-duplicate detection
    let mut duplicates: Vec<DuplicatePair> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();

    for (i, m) in memories.iter().enumerate() {
        if total > 100 && i % 50 == 0 {
            eprint!("\rChecking for duplicates... {}/{}", i, total);
        }

        let similar_results: Vec<SimilarResult> = ctx
            .recalld_json(&[
                "similar",
                &m.id,
                "--json",
                "--namespace",
                ctx.namespace(),
                "--limit",
                "5",
            ])
            .await
            .unwrap_or_default();

        for sr in &similar_results {
            if sr.similarity >= args.duplicate_threshold && sr.memory.id != m.id {
                let pair_key = if m.id < sr.memory.id {
                    (m.id.clone(), sr.memory.id.clone())
                } else {
                    (sr.memory.id.clone(), m.id.clone())
                };

                if seen_pairs.insert(pair_key) {
                    duplicates.push(DuplicatePair {
                        id_a: m.id.clone(),
                        id_b: sr.memory.id.clone(),
                        similarity: sr.similarity,
                        summary_a: truncate(&m.summary, 40),
                        summary_b: truncate(&sr.memory.summary, 40),
                    });
                }
            }
        }
    }
    if total > 100 {
        eprintln!();
    }

    // 2b. Orphaned memories
    let orphaned: Vec<OrphanedMemory> = memories
        .iter()
        .filter(|m| {
            let has_source_tag = m.tags.iter().any(|t| t.starts_with("source/"));
            has_source_tag && !manifest_id_set.contains(&m.id)
        })
        .map(|m| OrphanedMemory {
            id: m.id.clone(),
            summary: truncate(&m.summary, 60),
        })
        .collect();

    // 2c. Missing metadata
    let missing_metadata: Vec<MetadataIssue> = memories
        .iter()
        .filter_map(|m| {
            let mut missing = Vec::new();
            if m.entities.is_empty() {
                missing.push("entities".to_string());
            }
            if m.topics.is_empty() {
                missing.push("topics".to_string());
            }
            if !m.tags.iter().any(|t| t.starts_with("source/")) {
                missing.push("source_tag".to_string());
            }
            if missing.is_empty() {
                None
            } else {
                Some(MetadataIssue {
                    id: m.id.clone(),
                    summary: truncate(&m.summary, 60),
                    missing,
                })
            }
        })
        .collect();

    // 2d. Weak memories
    let decay_enabled = memories.iter().any(|m| m.strength < 1.0);
    let weak: Vec<WeakMemory> = if decay_enabled {
        memories
            .iter()
            .filter(|m| m.strength < args.weak_threshold)
            .map(|m| WeakMemory {
                id: m.id.clone(),
                summary: truncate(&m.summary, 60),
                strength: m.strength,
            })
            .collect()
    } else {
        Vec::new()
    };

    // 2e. Superseded chains
    let superseded_chains: Vec<SupersededChain> = memories
        .iter()
        .filter_map(|m| {
            m.supersedes.as_ref().and_then(|old_id| {
                if memory_id_set.contains(old_id) {
                    let old_summary = memories
                        .iter()
                        .find(|om| om.id == *old_id)
                        .map(|om| truncate(&om.summary, 40))
                        .unwrap_or_else(|| "(unknown)".to_string());
                    Some(SupersededChain {
                        old_id: old_id.clone(),
                        old_summary,
                        new_id: m.id.clone(),
                        new_summary: truncate(&m.summary, 40),
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    // 2f. Empty sources
    let empty_sources: Vec<String> = ctx
        .manifest()
        .sources
        .iter()
        .filter(|(_, entry)| entry.memory_ids.is_empty())
        .map(|(source, _)| source.clone())
        .collect();

    let warnings = duplicates.len()
        + orphaned.len()
        + missing_metadata.len()
        + weak.len()
        + superseded_chains.len()
        + empty_sources.len();

    let output = QualityOutput {
        agent: ctx.agent_name().to_string(),
        near_duplicates: duplicates,
        orphaned_memories: orphaned,
        missing_metadata,
        weak_memories: weak,
        superseded_chains,
        empty_sources,
        warnings,
        errors: 0,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        let bold = console::Style::new().bold();
        let green = console::Style::new().green();
        let yellow = console::Style::new().yellow();

        println!(
            "{}\n",
            bold.apply_to(format!("Quality Report for {}", ctx.agent_name()))
        );

        // Near-duplicates
        if output.near_duplicates.is_empty() {
            println!("  {} No near-duplicates found.", green.apply_to("[OK]"));
        } else {
            println!(
                "  {} {} near-duplicate pairs found (similarity > {:.2}):",
                yellow.apply_to("[WARN]"),
                output.near_duplicates.len(),
                args.duplicate_threshold,
            );
            for d in &output.near_duplicates {
                println!(
                    "    {} <-> {}  ({:.2})  \"{}\" / \"{}\"",
                    format_short_id(&d.id_a),
                    format_short_id(&d.id_b),
                    d.similarity,
                    d.summary_a,
                    d.summary_b,
                );
            }
        }
        println!();

        // Orphaned memories
        if output.orphaned_memories.is_empty() {
            println!(
                "  {} No orphaned memories found.",
                green.apply_to("[OK]")
            );
        } else {
            println!(
                "  {} {} orphaned memories found:",
                yellow.apply_to("[WARN]"),
                output.orphaned_memories.len(),
            );
            for o in &output.orphaned_memories {
                println!(
                    "    {}  \"{}\"",
                    format_short_id(&o.id),
                    o.summary,
                );
            }
        }
        println!();

        // Missing metadata
        if output.missing_metadata.is_empty() {
            println!(
                "  {} All memories have complete metadata.",
                green.apply_to("[OK]")
            );
        } else {
            let missing_entities: Vec<_> = output
                .missing_metadata
                .iter()
                .filter(|m| m.missing.contains(&"entities".to_string()))
                .collect();
            let missing_topics: Vec<_> = output
                .missing_metadata
                .iter()
                .filter(|m| m.missing.contains(&"topics".to_string()))
                .collect();
            let missing_source_tag: Vec<_> = output
                .missing_metadata
                .iter()
                .filter(|m| m.missing.contains(&"source_tag".to_string()))
                .collect();

            if !missing_entities.is_empty() {
                println!(
                    "  {} {} memories missing entities:",
                    yellow.apply_to("[WARN]"),
                    missing_entities.len(),
                );
                for m in &missing_entities {
                    println!("    {}  \"{}\"", format_short_id(&m.id), m.summary);
                }
            }
            if !missing_topics.is_empty() {
                println!(
                    "  {} {} memories missing topics:",
                    yellow.apply_to("[WARN]"),
                    missing_topics.len(),
                );
                for m in &missing_topics {
                    println!("    {}  \"{}\"", format_short_id(&m.id), m.summary);
                }
            }
            if !missing_source_tag.is_empty() {
                println!(
                    "  {} {} memories missing source tag:",
                    yellow.apply_to("[WARN]"),
                    missing_source_tag.len(),
                );
                for m in &missing_source_tag {
                    println!("    {}  \"{}\"", format_short_id(&m.id), m.summary);
                }
            }
        }
        println!();

        // Weak memories
        if !decay_enabled {
            println!(
                "  {} No weak memories found (decay disabled).",
                green.apply_to("[OK]")
            );
        } else if output.weak_memories.is_empty() {
            println!(
                "  {} No weak memories found (all above {:.2} threshold).",
                green.apply_to("[OK]"),
                args.weak_threshold,
            );
        } else {
            println!(
                "  {} {} weak memories (strength < {:.2}):",
                yellow.apply_to("[WARN]"),
                output.weak_memories.len(),
                args.weak_threshold,
            );
            for w in &output.weak_memories {
                println!(
                    "    {}  strength={:.2}  \"{}\"",
                    format_short_id(&w.id),
                    w.strength,
                    w.summary,
                );
            }
        }
        println!();

        // Superseded chains
        if output.superseded_chains.is_empty() {
            println!(
                "  {} No superseded chains found.",
                green.apply_to("[OK]")
            );
        } else {
            println!(
                "  {} {} superseded chains (old memory not forgotten):",
                yellow.apply_to("[WARN]"),
                output.superseded_chains.len(),
            );
            for sc in &output.superseded_chains {
                println!(
                    "    {} (old) -> {} (new)",
                    format_short_id(&sc.old_id),
                    format_short_id(&sc.new_id),
                );
                println!("      old: \"{}\"", sc.old_summary);
                println!("      new: \"{}\"", sc.new_summary);
            }
        }
        println!();

        // Empty sources
        if output.empty_sources.is_empty() {
            println!(
                "  {} All sources produced memories.",
                green.apply_to("[OK]")
            );
        } else {
            println!(
                "  {} {} empty sources (0 memories produced):",
                yellow.apply_to("[WARN]"),
                output.empty_sources.len(),
            );
            for s in &output.empty_sources {
                println!("    {}  (0 memories produced)", s);
            }
        }
        println!();

        // Summary
        println!(
            "  Summary: {} warnings, {} errors",
            output.warnings, output.errors
        );
    }

    // Interactive fix
    if args.fix && !json {
        execute_quality_fix(ctx, &output).await?;
    }

    Ok(())
}

async fn execute_quality_fix(
    ctx: &InspectContext,
    output: &QualityOutput,
) -> Result<(), CliError> {
    use dialoguer::Confirm;

    let bold = console::Style::new().bold();

    if !output.near_duplicates.is_empty() {
        println!("\n{}", bold.apply_to("Fix near-duplicates:"));
        for d in &output.near_duplicates {
            let msg = format!(
                "Merge {} <-> {} ({:.2})?\n  A: \"{}\"\n  B: \"{}\"",
                format_short_id(&d.id_a),
                format_short_id(&d.id_b),
                d.similarity,
                d.summary_a,
                d.summary_b,
            );

            if Confirm::new()
                .with_prompt(&msg)
                .default(false)
                .interact()
                .unwrap_or(false)
            {
                let _ = ctx
                    .recalld(&["forget", &d.id_a, "--namespace", ctx.namespace()])
                    .await;
                let _ = ctx
                    .recalld(&["forget", &d.id_b, "--namespace", ctx.namespace()])
                    .await;
                println!(
                    "  Removed {} and {}",
                    format_short_id(&d.id_a),
                    format_short_id(&d.id_b)
                );
            }
        }
    }

    if !output.orphaned_memories.is_empty() {
        println!("\n{}", bold.apply_to("Fix orphaned memories:"));
        for o in &output.orphaned_memories {
            let msg = format!(
                "Remove orphaned memory {}?\n  \"{}\"",
                format_short_id(&o.id),
                o.summary,
            );

            if Confirm::new()
                .with_prompt(&msg)
                .default(false)
                .interact()
                .unwrap_or(false)
            {
                let _ = ctx
                    .recalld(&["forget", &o.id, "--namespace", ctx.namespace()])
                    .await;
                println!("  Removed {}", format_short_id(&o.id));
            }
        }
    }

    if !output.superseded_chains.is_empty() {
        println!("\n{}", bold.apply_to("Fix superseded chains:"));
        for sc in &output.superseded_chains {
            let msg = format!(
                "Remove superseded memory {}?\n  old: \"{}\"\n  replaced by: {} \"{}\"",
                format_short_id(&sc.old_id),
                sc.old_summary,
                format_short_id(&sc.new_id),
                sc.new_summary,
            );

            if Confirm::new()
                .with_prompt(&msg)
                .default(false)
                .interact()
                .unwrap_or(false)
            {
                let _ = ctx
                    .recalld(&["forget", &sc.old_id, "--namespace", ctx.namespace()])
                    .await;
                println!("  Removed {}", format_short_id(&sc.old_id));
            }
        }
    }

    if !output.missing_metadata.is_empty() {
        println!(
            "\n  {} Missing metadata cannot be fixed interactively.",
            console::Style::new().dim().apply_to("Note:")
        );
        println!(
            "  Re-learn affected sources with `pupil build --no-cache` to regenerate memories."
        );
    }

    if !output.empty_sources.is_empty() {
        println!(
            "\n  {} Empty sources cannot be fixed interactively.",
            console::Style::new().dim().apply_to("Note:")
        );
        println!(
            "  Check that the source files are not empty and contain extractable content."
        );
    }

    Ok(())
}

// ── 6. graph subcommand ────────────────────────────────────────────

async fn execute_graph(
    ctx: &InspectContext,
    args: &GraphActionArgs,
    json: bool,
) -> Result<(), CliError> {
    let memories: Vec<Memory> = ctx
        .recalld_json(&["list", "--json", "--namespace", ctx.namespace()])
        .await?;

    let relationships: Vec<Relationship> = ctx
        .recalld_json(&[
            "relationships",
            "--all",
            "--json",
            "--namespace",
            ctx.namespace(),
        ])
        .await
        .unwrap_or_default();

    let memory_ids: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();

    if let Some(ref memory_id_short) = args.memory {
        return execute_graph_single(
            ctx,
            memory_id_short,
            args.depth,
            &relationships,
            json,
        )
        .await;
    }

    if args.dot {
        return execute_graph_dot(&memories, &relationships);
    }

    // Full graph summary
    let (num_components, components) = compute_components(&memory_ids, &relationships);

    let memories_with_rels: HashSet<String> = relationships
        .iter()
        .flat_map(|r| vec![r.from_id.clone(), r.to_id.clone()])
        .collect();
    let isolated: Vec<IsolatedMemory> = memories
        .iter()
        .filter(|m| !memories_with_rels.contains(&m.id))
        .map(|m| IsolatedMemory {
            id: m.id.clone(),
            summary: truncate(&m.summary, 60),
        })
        .collect();

    let mut edge_type_counts: HashMap<String, usize> = HashMap::new();
    for r in &relationships {
        *edge_type_counts
            .entry(r.relation_type.clone())
            .or_insert(0) += 1;
    }
    let total_edges = relationships.len();
    let mut edge_breakdown: Vec<EdgeTypeCount> = edge_type_counts
        .iter()
        .map(|(t, c)| EdgeTypeCount {
            edge_type: t.clone(),
            count: *c,
            share: if total_edges > 0 {
                *c as f64 / total_edges as f64 * 100.0
            } else {
                0.0
            },
        })
        .collect();
    edge_breakdown.sort_by(|a, b| b.count.cmp(&a.count));

    let largest_size = components.first().map(|c| c.len()).unwrap_or(0);
    let largest_center = components.first().and_then(|component| {
        let component_set: HashSet<&String> = component.iter().collect();
        let mut degree: HashMap<&String, usize> = HashMap::new();
        for r in &relationships {
            if component_set.contains(&r.from_id) {
                *degree.entry(&r.from_id).or_insert(0) += 1;
            }
            if component_set.contains(&r.to_id) {
                *degree.entry(&r.to_id).or_insert(0) += 1;
            }
        }
        degree
            .into_iter()
            .max_by_key(|(_, d)| *d)
            .map(|(id, _)| {
                memories
                    .iter()
                    .find(|m| m.id == *id)
                    .map(|m| truncate(&m.summary, 50))
                    .unwrap_or_else(|| id.clone())
            })
    });

    let graph_output = GraphOutput {
        agent: ctx.agent_name().to_string(),
        total_memories: memories.len(),
        total_relationships: relationships.len(),
        components: num_components,
        isolated: isolated.clone(),
        edge_type_breakdown: edge_breakdown.clone(),
        largest_component_size: largest_size,
        largest_component_center: largest_center.clone(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&graph_output).unwrap());
    } else {
        let bold = console::Style::new().bold();
        let dim = console::Style::new().dim();

        println!(
            "{}\n",
            bold.apply_to(format!("Knowledge Graph for {}", ctx.agent_name()))
        );

        println!("  {:<18}{}", bold.apply_to("Memories:"), memories.len());
        println!(
            "  {:<18}{}",
            bold.apply_to("Relationships:"),
            relationships.len()
        );
        println!(
            "  {:<18}{}  (connected subgraphs)",
            bold.apply_to("Components:"),
            num_components
        );
        println!(
            "  {:<18}{}  (memories with no relationships)",
            bold.apply_to("Isolated:"),
            isolated.len()
        );
        println!();

        if !edge_breakdown.is_empty() {
            println!("  {}:", bold.apply_to("Edge types"));
            for e in &edge_breakdown {
                println!(
                    "    {:<16}{:>4}  ({:>5.1}%)",
                    format!("{}:", e.edge_type),
                    e.count,
                    e.share,
                );
            }
            println!();
        }

        if largest_size > 0 {
            println!(
                "  Largest component: {} memories{}",
                largest_size,
                largest_center
                    .as_ref()
                    .map(|c| format!(" (centered around \"{}\")", c))
                    .unwrap_or_default()
            );
        }

        if !isolated.is_empty() {
            println!("  {}:", bold.apply_to("Isolated memories"));
            for iso in &isolated {
                println!(
                    "    {}  \"{}\"",
                    dim.apply_to(format_short_id(&iso.id)),
                    iso.summary,
                );
            }
        }
    }

    Ok(())
}

fn compute_components(
    memory_ids: &[String],
    relationships: &[Relationship],
) -> (usize, Vec<Vec<String>>) {
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for id in memory_ids {
        adj.entry(id.clone()).or_default();
    }
    for r in relationships {
        adj.entry(r.from_id.clone())
            .or_default()
            .push(r.to_id.clone());
        adj.entry(r.to_id.clone())
            .or_default()
            .push(r.from_id.clone());
    }

    let mut visited: HashSet<String> = HashSet::new();
    let mut components: Vec<Vec<String>> = Vec::new();

    for id in memory_ids {
        if visited.contains(id) {
            continue;
        }

        let mut component = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(id.clone());
        visited.insert(id.clone());

        while let Some(current) = queue.pop_front() {
            component.push(current.clone());
            if let Some(neighbors) = adj.get(&current) {
                for neighbor in neighbors {
                    if visited.insert(neighbor.clone()) {
                        queue.push_back(neighbor.clone());
                    }
                }
            }
        }

        components.push(component);
    }

    components.sort_by(|a, b| b.len().cmp(&a.len()));
    let count = components.len();
    (count, components)
}

async fn execute_graph_single(
    ctx: &InspectContext,
    memory_id_short: &str,
    depth: u32,
    all_relationships: &[Relationship],
    json: bool,
) -> Result<(), CliError> {
    let full_id = ctx.resolve_id(memory_id_short)?;
    let depth = depth.clamp(1, 3);

    let memory: Memory = ctx
        .recalld_json(&[
            "inspect",
            &full_id,
            "--json",
            "--namespace",
            ctx.namespace(),
        ])
        .await?;

    // Collect relationships within the depth via BFS expansion
    let mut relevant_rels: Vec<&Relationship> = Vec::new();
    let mut frontier: HashSet<String> = HashSet::new();
    frontier.insert(full_id.clone());

    for _ in 0..depth {
        let mut next_frontier = HashSet::new();
        for r in all_relationships {
            if frontier.contains(&r.from_id) || frontier.contains(&r.to_id) {
                if !relevant_rels.iter().any(|rr| std::ptr::eq(*rr, r)) {
                    relevant_rels.push(r);
                    next_frontier.insert(r.from_id.clone());
                    next_frontier.insert(r.to_id.clone());
                }
            }
        }
        frontier = next_frontier;
    }

    let my_rels: Vec<&Relationship> = relevant_rels
        .iter()
        .filter(|r| r.from_id == full_id || r.to_id == full_id)
        .copied()
        .collect();

    if json {
        let output = serde_json::json!({
            "memory": memory,
            "relationships": my_rels,
        });
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    } else {
        let bold = console::Style::new().bold();
        let dim = console::Style::new().dim();

        println!(
            "{}",
            bold.apply_to(format!(
                "Relationships for {} \"{}\"",
                format_short_id(&full_id),
                truncate(&memory.summary, 60)
            ))
        );
        println!();

        if my_rels.is_empty() {
            println!("  This memory has no relationships.");
            return Ok(());
        }

        let mut by_type: HashMap<String, Vec<&Relationship>> = HashMap::new();
        for r in &my_rels {
            let display_type = if r.from_id == full_id {
                r.relation_type.clone()
            } else {
                match r.relation_type.as_str() {
                    "parent" => "child".to_string(),
                    "child" => "parent".to_string(),
                    other => other.to_string(),
                }
            };
            by_type.entry(display_type).or_default().push(r);
        }

        let type_order = [
            "parent",
            "child",
            "associative",
            "entity",
            "supersedes",
            "superseded_by",
            "temporal",
            "causal",
            "contradicts",
        ];

        for rel_type in &type_order {
            if let Some(rels) = by_type.get(*rel_type) {
                let label = match *rel_type {
                    "parent" => "Parents:",
                    "child" => "Children:",
                    "associative" => "Associated:",
                    "entity" => "Entity links:",
                    "supersedes" => "Supersedes:",
                    "superseded_by" => "Superseded by:",
                    "temporal" => "Temporal:",
                    "causal" => "Causal:",
                    "contradicts" => "Contradicts:",
                    _ => *rel_type,
                };

                println!("  {}:", bold.apply_to(label));
                for r in rels {
                    let other_id = if r.from_id == full_id {
                        &r.to_id
                    } else {
                        &r.from_id
                    };

                    let arrow = relationship_arrow(rel_type);

                    let other_summary = ctx
                        .recalld_json::<Memory>(&[
                            "inspect",
                            other_id,
                            "--json",
                            "--namespace",
                            ctx.namespace(),
                        ])
                        .await
                        .map(|m| truncate(&m.summary, 50))
                        .unwrap_or_else(|_| "(unavailable)".to_string());

                    let via = r
                        .label
                        .as_ref()
                        .map(|l| format!("  (via: {})", l))
                        .unwrap_or_default();

                    println!(
                        "    {} {}  \"{}\"{}",
                        arrow,
                        dim.apply_to(format_short_id(other_id)),
                        other_summary,
                        dim.apply_to(&via),
                    );
                }
                println!();
            }
        }
    }

    Ok(())
}

fn execute_graph_dot(
    memories: &[Memory],
    relationships: &[Relationship],
) -> Result<(), CliError> {
    let palette = [
        "#c8e6c9", "#b3e5fc", "#fff9c4", "#f8bbd0", "#d1c4e9", "#ffe0b2", "#b2dfdb",
        "#f0f4c3",
    ];

    let mut source_colors: HashMap<String, &str> = HashMap::new();
    let mut color_idx = 0;
    for m in memories {
        let source = extract_source(&m.tags);
        source_colors.entry(source).or_insert_with(|| {
            let color = palette[color_idx % palette.len()];
            color_idx += 1;
            color
        });
    }

    println!("digraph knowledge {{");
    println!("  rankdir=LR;");
    println!(
        "  node [shape=box, style=\"rounded,filled\", fontsize=10];"
    );
    println!();

    println!("  // Nodes");
    for m in memories {
        let source = extract_source(&m.tags);
        let color = source_colors.get(&source).unwrap_or(&"#ffffff");
        let label = truncate(&m.summary, 40).replace('"', "\\\"");
        let short_id = m.id.replace('-', "");
        let node_id = &short_id[..8.min(short_id.len())];
        println!(
            "  \"{}\" [label=\"{}\", fillcolor=\"{}\"];",
            node_id, label, color
        );
    }
    println!();

    println!("  // Edges");
    for r in relationships {
        let from_short = r.from_id.replace('-', "");
        let to_short = r.to_id.replace('-', "");
        let from_node = &from_short[..8.min(from_short.len())];
        let to_node = &to_short[..8.min(to_short.len())];

        let (color, style) = match r.relation_type.as_str() {
            "parent" | "child" => ("#666666", "solid"),
            "supersedes" | "superseded_by" => ("#e53935", "solid"),
            "associative" => ("#999999", "dashed"),
            "entity" => ("#1e88e5", "dotted"),
            "temporal" | "causal" => ("#fb8c00", "solid"),
            "contradicts" => ("#e53935", "bold"),
            _ => ("#999999", "dashed"),
        };

        let label = r
            .label
            .as_ref()
            .map(|l| format!(", label=\"{}\"", l.replace('"', "\\\"")))
            .unwrap_or_default();

        println!(
            "  \"{}\" -> \"{}\" [color=\"{}\", style={}{}];",
            from_node, to_node, color, style, label
        );
    }

    println!("}}");

    Ok(())
}

// ── 7. diff subcommand ─────────────────────────────────────────────

async fn execute_diff(
    ctx: &InspectContext,
    args: &DiffActionArgs,
    source_filter: Option<&str>,
    json: bool,
) -> Result<(), CliError> {
    if args.tag_a == args.tag_b {
        println!("Cannot diff an image with itself.");
        return Ok(());
    }

    let agent_name = ctx.agent_name();
    let image_a = agent_config::image_ref(agent_name, Some(&args.tag_a));
    let image_b = agent_config::image_ref(agent_name, Some(&args.tag_b));

    let runtime = crate::container::detect()
        .map_err(|_| CliError::ContainerRuntimeNotFound)?;

    let container_a_id = start_diff_container(&*runtime, &image_a, &args.tag_a)
        .await
        .map_err(|_| CliError::ContainerRuntimeError {
            message: format!(
                "Image tag '{}' not found for agent '{}'. Run `pupil list` or check your container runtime for available tags.",
                args.tag_a, agent_name
            ),
        })?;

    let container_b_id = match start_diff_container(&*runtime, &image_b, &args.tag_b).await
    {
        Ok(id) => id,
        Err(_) => {
            let _ = runtime.rm(&container_a_id, true).await;
            return Err(CliError::ContainerRuntimeError {
                message: format!(
                    "Image tag '{}' not found for agent '{}'. Run `pupil list` or check your container runtime for available tags.",
                    args.tag_b, agent_name
                ),
            });
        }
    };

    let manifest_a = read_manifest_from_container(&*runtime, &container_a_id).await;
    let manifest_b = read_manifest_from_container(&*runtime, &container_b_id).await;

    let (manifest_a, manifest_b) = match (manifest_a, manifest_b) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            let _ = runtime.rm(&container_a_id, true).await;
            let _ = runtime.rm(&container_b_id, true).await;
            return Err(e);
        }
    };

    let ns = ctx.namespace();

    let mut added: Vec<DiffMemory> = Vec::new();
    let mut removed: Vec<DiffMemory> = Vec::new();
    let mut unchanged_count: usize = 0;
    let mut changed_sources: Vec<ChangedSource> = Vec::new();

    // Sources only in B (added)
    for (source, entry_b) in &manifest_b.sources {
        if let Some(filter) = source_filter {
            if !source.contains(filter) {
                continue;
            }
        }

        if !manifest_a.sources.contains_key(source) {
            for mid in &entry_b.memory_ids {
                let summary =
                    fetch_memory_summary(&*runtime, &container_b_id, mid, ns)
                        .await
                        .unwrap_or_else(|| "(details unavailable)".to_string());
                added.push(DiffMemory {
                    id: mid.clone(),
                    source: source.clone(),
                    summary,
                });
            }
            if !entry_b.memory_ids.is_empty() {
                changed_sources.push(ChangedSource {
                    source: source.clone(),
                    added: entry_b.memory_ids.len(),
                    removed: 0,
                    note: "(new source)".to_string(),
                });
            }
        }
    }

    // Sources only in A (removed)
    for (source, entry_a) in &manifest_a.sources {
        if let Some(filter) = source_filter {
            if !source.contains(filter) {
                continue;
            }
        }

        if !manifest_b.sources.contains_key(source) {
            for mid in &entry_a.memory_ids {
                let summary =
                    fetch_memory_summary(&*runtime, &container_a_id, mid, ns)
                        .await
                        .unwrap_or_else(|| "(details unavailable)".to_string());
                removed.push(DiffMemory {
                    id: mid.clone(),
                    source: source.clone(),
                    summary,
                });
            }
            if !entry_a.memory_ids.is_empty() {
                changed_sources.push(ChangedSource {
                    source: source.clone(),
                    added: 0,
                    removed: entry_a.memory_ids.len(),
                    note: "(source removed from curriculum)".to_string(),
                });
            }
        }
    }

    // Sources in both
    for (source, entry_a) in &manifest_a.sources {
        if let Some(filter) = source_filter {
            if !source.contains(filter) {
                continue;
            }
        }

        if let Some(entry_b) = manifest_b.sources.get(source) {
            if entry_a.content_hash == entry_b.content_hash {
                unchanged_count += entry_a.memory_ids.len();
            } else {
                let ids_a: HashSet<&String> = entry_a.memory_ids.iter().collect();
                let ids_b: HashSet<&String> = entry_b.memory_ids.iter().collect();

                let mut source_added = 0;
                let mut source_removed = 0;

                for mid in ids_b.difference(&ids_a) {
                    let summary =
                        fetch_memory_summary(&*runtime, &container_b_id, mid, ns)
                            .await
                            .unwrap_or_else(|| "(details unavailable)".to_string());
                    added.push(DiffMemory {
                        id: mid.to_string(),
                        source: source.clone(),
                        summary,
                    });
                    source_added += 1;
                }

                for mid in ids_a.difference(&ids_b) {
                    let summary =
                        fetch_memory_summary(&*runtime, &container_a_id, mid, ns)
                            .await
                            .unwrap_or_else(|| "(details unavailable)".to_string());
                    removed.push(DiffMemory {
                        id: mid.to_string(),
                        source: source.clone(),
                        summary,
                    });
                    source_removed += 1;
                }

                unchanged_count += ids_a.intersection(&ids_b).count();

                if source_added > 0 || source_removed > 0 {
                    changed_sources.push(ChangedSource {
                        source: source.clone(),
                        added: source_added,
                        removed: source_removed,
                        note: "(source content changed)".to_string(),
                    });
                }
            }
        }
    }

    changed_sources.sort_by(|a, b| a.source.cmp(&b.source));

    let diff_output = DiffOutput {
        agent: agent_name.to_string(),
        tag_a: args.tag_a.clone(),
        tag_b: args.tag_b.clone(),
        added: added.clone(),
        removed: removed.clone(),
        unchanged_count,
        changed_sources: changed_sources.clone(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&diff_output).unwrap());
    } else {
        let bold = console::Style::new().bold();
        let green = console::Style::new().green();
        let red = console::Style::new().red();
        let dim = console::Style::new().dim();

        println!(
            "{}\n",
            bold.apply_to(format!(
                "Diff: {}:{} -> {}:{}",
                agent_name, args.tag_a, agent_name, args.tag_b
            ))
        );

        if added.is_empty() && removed.is_empty() {
            println!("No differences found.");
        } else {
            if !added.is_empty() {
                println!(
                    "  {} ({} memories):",
                    bold.apply_to("Added"),
                    added.len()
                );
                for a in &added {
                    println!(
                        "    {} {}  {:<20} \"{}\"",
                        green.apply_to("+"),
                        dim.apply_to(format_short_id(&a.id)),
                        truncate(&a.source, 20),
                        truncate(&a.summary, 50),
                    );
                }
                println!();
            }

            if !removed.is_empty() {
                println!(
                    "  {} ({} memories):",
                    bold.apply_to("Removed"),
                    removed.len()
                );
                for r in &removed {
                    println!(
                        "    {} {}  {:<20} \"{}\"",
                        red.apply_to("-"),
                        dim.apply_to(format_short_id(&r.id)),
                        truncate(&r.source, 20),
                        truncate(&r.summary, 50),
                    );
                }
                println!();
            }

            if !changed_sources.is_empty() {
                println!("  {}:", bold.apply_to("Changed sources"));
                for cs in &changed_sources {
                    let change_str = match (cs.added, cs.removed) {
                        (a, 0) => format!("+{} memories", a),
                        (0, r) => format!("-{} memories", r),
                        (a, r) => format!("+{} / -{} memories", a, r),
                    };
                    println!(
                        "    {:<26} {} {}",
                        cs.source, change_str, cs.note
                    );
                }
                println!();
            }

            let unchanged_sources = ctx
                .manifest()
                .sources
                .len()
                .saturating_sub(changed_sources.len());
            println!(
                "  {}: {} memories across {} sources",
                dim.apply_to("Unchanged"),
                unchanged_count,
                unchanged_sources,
            );
            println!();

            println!(
                "  Summary: {} added, {} removed, {} unchanged",
                green.apply_to(format!("+{}", added.len())),
                red.apply_to(format!("-{}", removed.len())),
                unchanged_count,
            );
        }
    }

    let _ = runtime.rm(&container_a_id, true).await;
    let _ = runtime.rm(&container_b_id, true).await;

    Ok(())
}

async fn start_diff_container(
    runtime: &dyn ContainerRuntime,
    image_ref: &str,
    tag: &str,
) -> Result<ContainerId, CliError> {
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..6];
    let container_name = format!("pupil-diff-{}-{}", tag, suffix);

    let opts = RunOptions {
        name: Some(container_name),
        remove_on_exit: true,
        read_only: true,
        network: Some("none".to_string()),
        detach: true,
        entrypoint: Some("sleep".to_string()),
        command: vec!["infinity".to_string()],
        tmpfs: vec!["/tmp".to_string()],
        ..Default::default()
    };

    runtime
        .run(image_ref, &opts)
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to start container for {}: {}", image_ref, e),
        })
}

async fn read_manifest_from_container(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
) -> Result<Manifest, CliError> {
    let output = runtime
        .exec(
            container_id,
            &["cat", "/data/.pupil-manifest.json"],
            &[],
        )
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to read manifest: {e}"),
        })?;

    serde_json::from_str(&output.stdout).map_err(|e| CliError::ContainerRuntimeError {
        message: format!("Failed to parse manifest: {e}"),
    })
}

async fn fetch_memory_summary(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
    memory_id: &str,
    namespace: &str,
) -> Option<String> {
    let output = runtime
        .exec(
            container_id,
            &[
                "recalld",
                "inspect",
                memory_id,
                "--json",
                "--namespace",
                namespace,
            ],
            &[],
        )
        .await
        .ok()?;

    let memory: Memory = serde_json::from_str(&output.stdout).ok()?;
    Some(truncate(&memory.summary, 60))
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_short_id() {
        assert_eq!(
            format_short_id("3a7fc2d8-1234-4567-890a-bcdef1234567"),
            "3a7f..67"
        );
        assert_eq!(
            format_short_id("abcdef01-2345-6789-abcd-ef0123456789"),
            "abcd..89"
        );
        assert_eq!(format_short_id("abcd"), "abcd");
        assert_eq!(format_short_id("ab"), "ab");
    }

    #[test]
    fn test_extract_source() {
        assert_eq!(
            extract_source(&[
                "source/handbook.md".to_string(),
                "type/procedure".to_string()
            ]),
            "handbook.md"
        );
        assert_eq!(
            extract_source(&["type/procedure".to_string()]),
            "unknown"
        );
        assert_eq!(extract_source(&[]), "unknown");
        assert_eq!(
            extract_source(&[
                "type/concept".to_string(),
                "source/runbooks/deploy.md".to_string()
            ]),
            "runbooks/deploy.md"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello world", 20), "hello world");
        assert_eq!(truncate("hello world", 8), "hello...");
        assert_eq!(truncate("hello world", 5), "he...");
        assert_eq!(truncate("hello world", 3), "...");
        assert_eq!(truncate("hello world", 2), "..");
        assert_eq!(truncate("hello world", 1), ".");
        assert_eq!(truncate("hello world", 0), "");
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_filter_by_source() {
        let memories = vec![
            Memory {
                id: "1".into(),
                tags: vec!["source/handbook.md".into()],
                ..test_memory()
            },
            Memory {
                id: "2".into(),
                tags: vec!["source/api-ref.md".into()],
                ..test_memory()
            },
            Memory {
                id: "3".into(),
                tags: vec!["source/handbook.md".into()],
                ..test_memory()
            },
        ];

        let filtered = filter_by_source(&memories, Some("handbook"));
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "1");
        assert_eq!(filtered[1].id, "3");

        let filtered = filter_by_source(&memories, Some("api"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "2");

        let filtered = filter_by_source(&memories, None);
        assert_eq!(filtered.len(), 3);

        let filtered = filter_by_source(&memories, Some("nonexistent"));
        assert_eq!(filtered.len(), 0);
    }

    #[test]
    fn test_filter_by_tag() {
        let memories = vec![
            Memory {
                id: "1".into(),
                tags: vec!["type/procedure".into(), "source/a.md".into()],
                ..test_memory()
            },
            Memory {
                id: "2".into(),
                tags: vec!["type/reference".into(), "source/b.md".into()],
                ..test_memory()
            },
            Memory {
                id: "3".into(),
                tags: vec!["type/procedure".into(), "source/c.md".into()],
                ..test_memory()
            },
        ];

        let filtered = filter_by_tag(&memories, Some("type/procedure"));
        assert_eq!(filtered.len(), 2);

        let filtered = filter_by_tag(&memories, Some("type/reference"));
        assert_eq!(filtered.len(), 1);

        let filtered = filter_by_tag(&memories, None);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_compute_components() {
        let ids = vec!["a".into(), "b".into(), "c".into()];
        let rels = vec![Relationship {
            from_id: "a".into(),
            to_id: "b".into(),
            relation_type: "associative".into(),
            label: None,
        }];

        let (count, components) = compute_components(&ids, &rels);
        assert_eq!(count, 2);
        assert_eq!(components[0].len(), 2);
        assert_eq!(components[1].len(), 1);
    }

    #[test]
    fn test_compute_components_all_connected() {
        let ids = vec!["a".into(), "b".into(), "c".into()];
        let rels = vec![
            Relationship {
                from_id: "a".into(),
                to_id: "b".into(),
                relation_type: "parent".into(),
                label: None,
            },
            Relationship {
                from_id: "b".into(),
                to_id: "c".into(),
                relation_type: "associative".into(),
                label: None,
            },
        ];

        let (count, components) = compute_components(&ids, &rels);
        assert_eq!(count, 1);
        assert_eq!(components[0].len(), 3);
    }

    #[test]
    fn test_compute_components_empty() {
        let ids: Vec<String> = vec![];
        let rels: Vec<Relationship> = vec![];
        let (count, components) = compute_components(&ids, &rels);
        assert_eq!(count, 0);
        assert_eq!(components.len(), 0);
    }

    #[test]
    fn test_relationship_arrows() {
        assert_eq!(relationship_arrow("parent"), "->");
        assert_eq!(relationship_arrow("associative"), "<>");
        assert_eq!(relationship_arrow("entity"), "~~");
        assert_eq!(relationship_arrow("supersedes"), "=>");
        assert_eq!(relationship_arrow("contradicts"), "><");
    }

    fn test_memory() -> Memory {
        Memory {
            id: "00000000-0000-0000-0000-000000000000".into(),
            summary: "test memory".into(),
            full_text: None,
            entities: vec![],
            topics: vec![],
            tags: vec![],
            strength: 1.0,
            phase: "full".into(),
            created_at: "2026-06-25T00:00:00Z".into(),
            accessed_at: "2026-06-25T00:00:00Z".into(),
            parent_id: None,
            supersedes: None,
            namespace: "knowledge".into(),
        }
    }
}

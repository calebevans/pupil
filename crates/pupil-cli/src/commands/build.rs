use clap::Args;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::agent_config::{
    resolve_agent, resolve_api_key, image_ref, lookup_pricing, AgentConfig, SourceEntry,
    BASE_IMAGE,
};
use crate::container::{self, ContainerId, ContainerRuntime, RunOptions};
use crate::error::CliError;

use super::teach::is_supported_curriculum_file;

#[derive(Args, Debug)]
pub struct BuildArgs {
    pub name: Option<String>,

    #[arg(long)]
    pub no_cache: bool,

    #[arg(long)]
    pub no_confirm: bool,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub runtime: Option<String>,

    #[arg(long, default_value = "auto")]
    pub progress: String,

    #[arg(long)]
    pub tag: Option<String>,
}

#[derive(Debug, Clone)]
struct SourceItem {
    key: String,
    path: Option<PathBuf>,
    #[allow(dead_code)]
    url: Option<String>,
    content_size: u64,
}

struct BuildSummary {
    memories_created: u64,
    input_tokens: u64,
    output_tokens: u64,
    input_cost: f64,
    output_cost: f64,
}

struct FailureAnalysis {
    /// Source file paths collected from failed test cases.
    failed_sources: Vec<String>,
    /// Number of tests that passed.
    pass_count: usize,
    /// Number of tests that failed.
    #[allow(dead_code)]
    fail_count: usize,
    /// Total number of tests.
    total: usize,
    /// Pass rate as a fraction (0.0 - 1.0).
    pass_rate: f64,
}

pub async fn execute(args: BuildArgs) -> Result<(), CliError> {
    let start_time = Instant::now();

    let (agent_dir, config) = resolve_agent(args.name.as_deref())?;
    let agent_dir = agent_dir.canonicalize().map_err(|e| CliError::Io(e))?;
    let curriculum_dir = agent_dir.join("curriculum");

    let runtime = detect_or_override_runtime(args.runtime.as_deref()).await?;

    let learning_model = config.learning_model.as_deref().unwrap_or(&config.model);
    let (key_name, key_set) = resolve_api_key(learning_model);
    if !key_set {
        return Err(CliError::EnvVarMissing { name: key_name });
    }

    if !curriculum_dir.exists() || dir_is_empty(&curriculum_dir)? {
        let has_url_sources = config.curriculum.sources.iter().any(|s| match s {
            SourceEntry::Short(s) => s.starts_with("http"),
            SourceEntry::Long(sc) => sc.url.is_some(),
        });
        if !has_url_sources {
            return Err(CliError::NoCurriculumSources {
                path: curriculum_dir,
            });
        }
    }

    // ---- Phase 1: Scan curriculum ----
    let multi_progress = MultiProgress::new();
    let scan_spinner = multi_progress.add(ProgressBar::new_spinner());
    scan_spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    scan_spinner.set_message("Scanning curriculum...");

    let sources = enumerate_sources(&config, &agent_dir)?;

    let image_name = image_ref(&config.name, args.tag.as_deref());
    let manifest = if args.no_cache {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        load_manifest_from_image(runtime.as_ref(), &image_name)
            .await
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    };

    let manifest_sources = manifest
        .get("sources")
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_default();

    let mut to_learn: Vec<SourceItem> = Vec::new();
    let mut unchanged: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();

    for source in &sources {
        let hash = hash_source(source)?;
        if let Some(existing) = manifest_sources.get(&source.key) {
            if let Some(existing_hash) = existing.get("content_hash").and_then(|h| h.as_str()) {
                if existing_hash == hash {
                    unchanged.push(source.key.clone());
                    continue;
                }
            }
        }
        to_learn.push(source.clone());
    }

    for key in manifest_sources.keys() {
        if !sources.iter().any(|s| s.key == *key) {
            removed.push(key.clone());
        }
    }

    scan_spinner.finish_with_message(format!(
        "Scanning curriculum... {} source(s) found ({} unchanged, skipping)",
        sources.len(),
        unchanged.len()
    ));

    // ---- Dry run ----
    if args.dry_run {
        println!("Dry run: no learning will be performed.");
        println!();
        println!(
            "  Sources to learn: {} ({} unchanged, will skip)",
            to_learn.len(),
            unchanged.len()
        );
        if !removed.is_empty() {
            println!(
                "  Sources removed:  {} (memories will be cleaned up)",
                removed.len()
            );
        }

        let total_chars: u64 = to_learn.iter().map(|s| s.content_size).sum();
        let estimated_tokens = (total_chars / 4) * 3;
        println!(
            "  Estimated input tokens: ~{} (heuristic, +-15%)",
            format_number(estimated_tokens)
        );

        let (input_price, output_price) = lookup_pricing(learning_model);
        let estimated_cost_low = (estimated_tokens as f64 / 1_000_000.0) * input_price;
        let estimated_cost_high = estimated_cost_low * 5.0;
        println!(
            "  Estimated cost: ${:.2} - ${:.2} ({})",
            estimated_cost_low, estimated_cost_high, learning_model
        );

        if let Some(budget) = config.build.max_cost_usd {
            println!("  Budget: ${:.2} (max_cost_usd)", budget);
        }

        println!();
        println!("  Note: actual cost depends on agent behavior (number of");
        println!("  memories created, deduplication checks, linking). The");
        println!("  estimate uses a 3x overhead factor for tool-call turns.");
        let _ = output_price;
        return Ok(());
    }

    if to_learn.is_empty() && removed.is_empty() {
        println!("Nothing to build. All sources are unchanged.");
        return Ok(());
    }

    // ---- Phase 2: Start build infrastructure ----
    let container_spinner = multi_progress.add(ProgressBar::new_spinner());
    container_spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    container_spinner.set_message("Starting build container...");

    // Pull base image if needed
    if !image_exists(runtime.as_ref(), BASE_IMAGE).await? {
        container_spinner.set_message("Pulling base image...");
        runtime
            .pull(BASE_IMAGE)
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to pull base image: {}", e),
            })?;
    }

    // Check if Ollama is running on the host
    let host_ollama_url = std::env::var("OLLAMA_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:11434".to_string());
    let use_host_ollama = reqwest::get(&format!("{}/api/tags", host_ollama_url))
        .await
        .is_ok();

    // Set up build network and Ollama sidecar
    let build_network = format!("pupil-build-{}-net", config.name);
    let ollama_container_name =
        format!("pupil-build-{}-ollama-{}", config.name, uuid_short());
    let mut ollama_container_id: Option<ContainerId> = None;

    if use_host_ollama {
        container_spinner.set_message("Using host Ollama for embeddings...");
    } else {
        create_network(runtime.as_ref(), &build_network).await?;

        container_spinner.set_message("Starting Ollama sidecar...");

        let ollama_id = runtime
            .run(
                "ollama/ollama:latest",
                &RunOptions {
                    name: Some(ollama_container_name.clone()),
                    detach: true,
                    network: Some(build_network.clone()),
                    volumes: vec!["pupil-ollama-models:/root/.ollama".to_string()],
                    env: HashMap::from([
                        ("OLLAMA_NUM_PARALLEL".to_string(), "4".to_string()),
                        ("OLLAMA_MAX_LOADED_MODELS".to_string(), "1".to_string()),
                    ]),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to start Ollama sidecar: {}", e),
            })?;

        container_spinner.set_message("Waiting for Ollama...");
        wait_for_ollama_in_container(runtime.as_ref(), &ollama_id, 120).await?;

        container_spinner.set_message("Pulling embedding model...");
        runtime
            .exec(&ollama_id, &["ollama", "pull", "embeddinggemma"], &[])
            .await
            .map_err(|e| CliError::ContainerRuntimeError {
                message: format!("Failed to pull embedding model: {}", e),
            })?;

        ollama_container_id = Some(ollama_id);
    }

    // Create and initialize the data volume
    let data_volume_name = format!("pupil-build-{}-data", config.name);
    create_data_volume(runtime.as_ref(), &data_volume_name).await?;

    // Build environment variables
    let mut build_env = HashMap::new();

    // Primary LLM API key
    let api_key_value = std::env::var(&key_name).unwrap_or_default();
    build_env.insert(key_name.clone(), api_key_value);

    // Pass through additional API keys (embedding providers may need these)
    for var in &["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
        if *var != key_name {
            if let Ok(val) = std::env::var(var) {
                build_env.insert(var.to_string(), val);
            }
        }
    }

    // Cloud provider credentials
    for var in &[
        "VERTEX_PROJECT_ID",
        "VERTEX_LOCATION",
        "VERTEX_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AZURE_OPENAI_API_KEY",
        "AZURE_OPENAI_ENDPOINT",
    ] {
        if let Ok(val) = std::env::var(var) {
            build_env.insert(var.to_string(), val);
        }
    }

    // Judge model API key (may differ from agent model)
    if let Some(ref self_test) = config.build.self_test {
        if let Some(ref judge_model) = self_test.judge_model {
            let (judge_key_name, judge_key_set) = resolve_api_key(judge_model);
            if judge_key_set {
                if let Ok(val) = std::env::var(&judge_key_name) {
                    build_env.insert(judge_key_name, val);
                }
            }
            // Also pass the judge model identifier so the in-container
            // test runner can construct a provider for it.
            build_env.insert(
                "PUPIL_JUDGE_MODEL".to_string(),
                judge_model.clone(),
            );
        }
    }

    // recalld embedding configuration
    let container_ollama_url = if use_host_ollama {
        "http://host.docker.internal:11434".to_string()
    } else {
        format!("http://{}:11434", ollama_container_name)
    };
    build_env.insert(
        "RECALLD_EMBEDDING_PROVIDER".to_string(),
        "ollama".to_string(),
    );
    build_env.insert(
        "RECALLD_EMBEDDING_MODEL".to_string(),
        "embeddinggemma:latest".to_string(),
    );
    build_env.insert(
        "RECALLD_EMBEDDING_BASE_URL".to_string(),
        container_ollama_url,
    );
    build_env.insert(
        "RECALLD_EMBEDDING_DIMENSIONS".to_string(),
        "768".to_string(),
    );
    build_env.insert("PUPIL_LOG_FORMAT".to_string(), "json".to_string());

    // Build volumes list
    let mut volumes = vec![
        format!("{}:/data", data_volume_name),
        format!("{}:/curriculum:ro", curriculum_dir.to_string_lossy()),
        format!(
            "{}:/tmp/pupil.yaml:ro",
            agent_dir.join("pupil.yaml").to_string_lossy()
        ),
    ];

    if let Some(ref self_test) = config.build.self_test {
        let test_file = agent_dir.join(&self_test.file);
        if test_file.exists() {
            volumes.push(format!(
                "{}:/tmp/_pupil_test_suite.yaml:ro",
                test_file.to_string_lossy()
            ));
        }
    }

    // Build uses `docker run` (not exec) so output goes to docker logs.
    // Each step runs as the container's main process, making logs visible
    // via `docker logs <name>` from another terminal.
    let build_env_vec: Vec<String> = build_env
        .iter()
        .flat_map(|(k, v)| vec!["-e".to_string(), format!("{}={}", k, v)])
        .collect();
    let volume_flags: Vec<String> = volumes
        .iter()
        .flat_map(|v| vec!["-v".to_string(), v.clone()])
        .collect();
    let mut base_run_args: Vec<String> = Vec::new();
    base_run_args.extend(volume_flags);
    base_run_args.extend(build_env_vec);
    if use_host_ollama {
        base_run_args.push("--add-host".to_string());
        base_run_args.push("host.docker.internal:host-gateway".to_string());
    } else {
        base_run_args.push("--network".to_string());
        base_run_args.push(build_network.clone());
    }

    container_spinner.finish_with_message("Build environment ready.");

    // ---- Phase 3: Forget removed sources ----
    for source_key in &removed {
        let forget_spinner = multi_progress.add(ProgressBar::new_spinner());
        forget_spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.cyan} {msg}")
                .unwrap(),
        );
        forget_spinner.set_message(format!("Cleaning up: {}", source_key));

        let mut args = vec!["run".to_string(), "--rm".to_string()];
        args.extend(base_run_args.clone());
        args.push(BASE_IMAGE.to_string());
        args.push("--config".to_string());
        args.push("/tmp/pupil.yaml".to_string());
        args.push("forget-source".to_string());
        args.push(source_key.to_string());

        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let result = crate::container::execute_runtime_command(
            runtime.binary_path(),
            "run",
            &refs,
        )
        .await;

        if let Err(e) = result {
            cleanup_build_simple(
                runtime.as_ref(),
                ollama_container_id.as_ref(),
                &data_volume_name,
                &build_network,
            )
            .await;
            return Err(CliError::ContainerRuntimeError {
                message: format!("Failed to forget source '{}': {}", source_key, e),
            });
        }

        forget_spinner.finish_with_message(format!("Cleaned up: {}", source_key));
    }

    // ---- Phase 4: Learn curriculum ----
    // Run as the container's main process so output is visible in docker logs.
    let learn_container_name = format!("pupil-learn-{}-{}", config.name, uuid_short());

    println!("Learning curriculum... (watch logs: docker logs -f {})", learn_container_name);

    let mut learn_run_args = vec![
        "run".to_string(),
        "--name".to_string(),
        learn_container_name.clone(),
    ];
    learn_run_args.extend(base_run_args.clone());
    learn_run_args.push(BASE_IMAGE.to_string());
    learn_run_args.push("--config".to_string());
    learn_run_args.push("/tmp/pupil.yaml".to_string());
    learn_run_args.push("learn".to_string());

    let learn_refs: Vec<&str> = learn_run_args.iter().map(|s| s.as_str()).collect();
    let learn_result = crate::container::execute_runtime_command(
        runtime.binary_path(),
        "run",
        &learn_refs,
    )
    .await;

    let container_id = ContainerId(learn_container_name.clone());

    match learn_result {
        Ok(_) => {}
        Err(e) => {
            let _ = runtime.rm(&container_id, true).await;
            cleanup_build_simple(
                runtime.as_ref(),
                ollama_container_id.as_ref(),
                &data_volume_name,
                &build_network,
            )
            .await;
            return Err(CliError::ContainerRuntimeError {
                message: format!("Learning failed: {}", e),
            });
        }
    }

    println!("Learning complete.");

    // ---- Phase 5: Self-test cycle ----
    if let Some(ref self_test) = config.build.self_test {
        let test_file = agent_dir.join(&self_test.file);
        if !test_file.exists() {
            cleanup_build(
                runtime.as_ref(),
                &container_id,
                ollama_container_id.as_ref(),
                &data_volume_name,
                &build_network,
            )
            .await;
            return Err(CliError::ConfigInvalid {
                message: format!(
                    "Self-test file '{}' not found at {}",
                    self_test.file,
                    test_file.display()
                ),
            });
        }

        let mut retries = 0u32;
        loop {
            // Phase 5a: Run test suite (test file is bind-mounted)
            let test_spinner = multi_progress.add(ProgressBar::new_spinner());
            test_spinner.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")
                    .unwrap(),
            );
            test_spinner.set_message(format!(
                "Self-test: running tests (attempt {})...",
                retries + 1
            ));

            let judge_model = config
                .build
                .self_test
                .as_ref()
                .and_then(|st| st.judge_model.as_deref());

            let test_result = run_self_test_in_container(
                runtime.as_ref(),
                &base_run_args,
                &config.name,
                judge_model,
            )
            .await;

            let test_result = match test_result {
                Ok(result) => result,
                Err(e) => {
                    test_spinner.finish_with_message("Self-test: execution failed.");
                    cleanup_build(
                        runtime.as_ref(),
                        &container_id,
                        ollama_container_id.as_ref(),
                        &data_volume_name,
                        &build_network,
                    )
                    .await;
                    return Err(e);
                }
            };

            // Phase 5b: Analyze results
            let analysis = analyze_test_failures(&test_file, &test_result)?;

            if analysis.pass_rate >= self_test.min_score {
                test_spinner.finish_with_message(format!(
                    "Self-test: {}/{} passed ({:.0}%) -- above threshold ({:.0}%)",
                    analysis.pass_count,
                    analysis.total,
                    analysis.pass_rate * 100.0,
                    self_test.min_score * 100.0,
                ));
                break;
            }

            retries += 1;
            if retries >= self_test.max_retries {
                test_spinner.finish_with_message(format!(
                    "Self-test: {}/{} passed ({:.0}%) -- below threshold after {} retries",
                    analysis.pass_count,
                    analysis.total,
                    analysis.pass_rate * 100.0,
                    self_test.max_retries,
                ));

                let fail_detail = format!(
                    "Self-test failed: {:.0}% pass rate after {} remedial cycle(s) \
                     (threshold: {:.0}%)\nFailed sources: {}",
                    analysis.pass_rate * 100.0,
                    self_test.max_retries,
                    self_test.min_score * 100.0,
                    if analysis.failed_sources.is_empty() {
                        "(none identified)".to_string()
                    } else {
                        analysis.failed_sources.join(", ")
                    },
                );

                cleanup_build(
                    runtime.as_ref(),
                    &container_id,
                    ollama_container_id.as_ref(),
                    &data_volume_name,
                    &build_network,
                )
                .await;
                return Err(CliError::LearningFailed {
                    exit_code: 1,
                    stderr: fail_detail,
                });
            }

            test_spinner.finish_with_message(format!(
                "Self-test: {}/{} passed ({:.0}%) -- below threshold ({:.0}%), \
                 starting remedial learning (attempt {}/{})...",
                analysis.pass_count,
                analysis.total,
                analysis.pass_rate * 100.0,
                self_test.min_score * 100.0,
                retries,
                self_test.max_retries,
            ));

            // Log failed sources for visibility
            if !analysis.failed_sources.is_empty() {
                for source in &analysis.failed_sources {
                    tracing::info!(source = %source, "Source targeted for re-reading");
                }
            }

            // Phase 5c: Remedial learning (test questions are NOT in the prompt)
            if let Err(e) = run_remedial_learning(
                runtime.as_ref(),
                &base_run_args,
                &config.name,
                &analysis,
            )
            .await
            {
                cleanup_build(
                    runtime.as_ref(),
                    &container_id,
                    ollama_container_id.as_ref(),
                    &data_volume_name,
                    &build_network,
                )
                .await;
                return Err(e);
            }
        }
    }

    // ---- Phase 6: Commit image ----
    let commit_spinner = multi_progress.add(ProgressBar::new_spinner());
    commit_spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    commit_spinner.set_message("Committing image...");

    let tag = args.tag.as_deref().unwrap_or("latest");
    let final_image = image_ref(&config.name, Some(tag));

    let commit_result = runtime.commit(&container_id, &final_image).await;

    if let Err(e) = commit_result {
        cleanup_build(
            runtime.as_ref(),
            &container_id,
            ollama_container_id.as_ref(),
            &data_volume_name,
            &build_network,
        )
        .await;
        return Err(CliError::CommitFailed {
            message: e.to_string(),
        });
    }

    commit_spinner.finish_with_message(format!("Image committed: {}", final_image));

    // ---- Phase 7: Cleanup ----
    cleanup_build(
        runtime.as_ref(),
        &container_id,
        ollama_container_id.as_ref(),
        &data_volume_name,
        &build_network,
    )
    .await;

    // ---- Phase 8: Summary ----
    let elapsed = start_time.elapsed();

    let build_summary: Option<BuildSummary> = None; // TODO: extract from docker logs in future

    println!();
    println!("Build complete.");
    println!(
        "  Sources learned: {} ({} skipped, unchanged)",
        to_learn.len(),
        unchanged.len()
    );
    if !removed.is_empty() {
        println!("  Sources removed: {}", removed.len());
    }
    if let Some(ref summary) = build_summary {
        println!("  Memories created: {}", summary.memories_created);
        println!("  Token usage:");
        println!(
            "    Input:  {} tokens (${:.2})",
            format_number(summary.input_tokens),
            summary.input_cost
        );
        println!(
            "    Output: {} tokens (${:.2})",
            format_number(summary.output_tokens),
            summary.output_cost
        );
        println!(
            "    Total:  {} tokens (${:.2})",
            format_number(summary.input_tokens + summary.output_tokens),
            summary.input_cost + summary.output_cost
        );
    }
    println!(
        "  Image: {} ({:.1}s)",
        final_image,
        elapsed.as_secs_f64()
    );
    println!("  Duration: {:.1}s", elapsed.as_secs_f64());

    Ok(())
}

async fn create_data_volume(
    runtime: &dyn ContainerRuntime,
    volume_name: &str,
) -> Result<(), CliError> {
    // Create the volume
    let _ = tokio::process::Command::new(runtime.binary_path())
        .args(["volume", "create", volume_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    // Initialize directory structure and permissions for UID 65532.
    // Use the base image itself (already pulled) with --user root to
    // create directories owned by the nonroot user. The busybox-static
    // binary in distroless provides mkdir and chown.
    // Fallback: if that fails, try with alpine (widely available).
    let init_result = tokio::process::Command::new(runtime.binary_path())
        .args([
            "run", "--rm", "--user", "root",
            "-v", &format!("{}:/data", volume_name),
            "--entrypoint", "/busybox/sh",
            BASE_IMAGE,
            "-c", "mkdir -p /data/recalld /data/sessions && chown -R 65532:65532 /data",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;

    match init_result {
        Ok(s) if s.success() => return Ok(()),
        _ => {}
    }

    // Fallback: distroless may not have busybox. Try alpine.
    let fallback = tokio::process::Command::new(runtime.binary_path())
        .args([
            "run", "--rm",
            "-v", &format!("{}:/data", volume_name),
            "alpine",
            "sh", "-c",
            "mkdir -p /data/recalld /data/sessions && chown -R 65532:65532 /data",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to initialize data volume: {}", e),
        })?;

    if !fallback.success() {
        return Err(CliError::ContainerRuntimeError {
            message: "Failed to set data volume permissions. Ensure Docker is running.".to_string(),
        });
    }

    Ok(())
}

async fn create_network(
    runtime: &dyn ContainerRuntime,
    name: &str,
) -> Result<(), CliError> {
    let output = tokio::process::Command::new(runtime.binary_path())
        .args(["network", "create", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to create network: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("already exists") {
            return Err(CliError::ContainerRuntimeError {
                message: format!("Failed to create network '{}': {}", name, stderr),
            });
        }
    }
    Ok(())
}

async fn remove_network(runtime: &dyn ContainerRuntime, name: &str) {
    let _ = tokio::process::Command::new(runtime.binary_path())
        .args(["network", "rm", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
}

async fn wait_for_ollama_in_container(
    runtime: &dyn ContainerRuntime,
    id: &ContainerId,
    timeout_secs: u64,
) -> Result<(), CliError> {
    let start = Instant::now();
    loop {
        if start.elapsed().as_secs() > timeout_secs {
            return Err(CliError::OllamaNotReachable {
                url: "ollama sidecar container".to_string(),
            });
        }
        let result = runtime
            .exec(
                id,
                &["curl", "-sf", "http://localhost:11434/api/tags"],
                &[],
            )
            .await;
        match result {
            Ok(output) if output.exit_code == 0 => return Ok(()),
            _ => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
        }
    }
}

async fn cleanup_build(
    runtime: &dyn ContainerRuntime,
    build_container_id: &ContainerId,
    ollama_container_id: Option<&ContainerId>,
    data_volume_name: &str,
    build_network: &str,
) {
    let _ = runtime.rm(build_container_id, true).await;
    if let Some(ollama_id) = ollama_container_id {
        let _ = runtime.rm(ollama_id, true).await;
    }
    let _ = tokio::process::Command::new(runtime.binary_path())
        .args(["volume", "rm", "-f", data_volume_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    remove_network(runtime, build_network).await;
}

async fn cleanup_build_simple(
    runtime: &dyn ContainerRuntime,
    ollama_container_id: Option<&ContainerId>,
    data_volume_name: &str,
    build_network: &str,
) {
    if let Some(ollama_id) = ollama_container_id {
        let _ = runtime.rm(ollama_id, true).await;
    }
    let _ = tokio::process::Command::new(runtime.binary_path())
        .args(["volume", "rm", "-f", data_volume_name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await;
    remove_network(runtime, build_network).await;
}

async fn detect_or_override_runtime(
    runtime_override: Option<&str>,
) -> Result<Box<dyn ContainerRuntime>, CliError> {
    if let Some(rt) = runtime_override {
        match rt {
            "docker" => {
                let path = which::which("docker").map_err(|_| CliError::ContainerRuntimeNotFound)?;
                Ok(Box::new(container::DockerRuntime::new(path)))
            }
            "podman" => {
                let path = which::which("podman").map_err(|_| CliError::ContainerRuntimeNotFound)?;
                Ok(Box::new(container::PodmanRuntime::new(path)))
            }
            other => Err(CliError::ConfigInvalid {
                message: format!("Unknown runtime: '{}'. Use 'docker' or 'podman'.", other),
            }),
        }
    } else {
        container::detect().map_err(|_| CliError::ContainerRuntimeNotFound)
    }
}

async fn image_exists(
    runtime: &dyn ContainerRuntime,
    image: &str,
) -> Result<bool, CliError> {
    let output = tokio::process::Command::new(runtime.binary_path())
        .args(["image", "inspect", image])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    Ok(output.success())
}

fn dir_is_empty(dir: &std::path::Path) -> Result<bool, CliError> {
    Ok(std::fs::read_dir(dir)?.next().is_none())
}

fn enumerate_sources(
    config: &AgentConfig,
    agent_dir: &std::path::Path,
) -> Result<Vec<SourceItem>, CliError> {
    let mut items = Vec::new();

    for source in &config.curriculum.sources {
        match source {
            SourceEntry::Short(s) => {
                if s.starts_with("http://") || s.starts_with("https://") {
                    items.push(SourceItem {
                        key: s.clone(),
                        path: None,
                        url: Some(s.clone()),
                        content_size: 0,
                    });
                } else {
                    let resolved = agent_dir.join(s);
                    if resolved.is_dir() {
                        collect_source_files(&resolved, agent_dir, &mut items)?;
                    } else if resolved.is_file() {
                        let size = std::fs::metadata(&resolved)?.len();
                        let key = resolved
                            .strip_prefix(agent_dir)
                            .unwrap_or(&resolved)
                            .to_string_lossy()
                            .to_string();
                        items.push(SourceItem {
                            key,
                            path: Some(resolved),
                            url: None,
                            content_size: size,
                        });
                    }
                }
            }
            SourceEntry::Long(sc) => {
                if let Some(ref url) = sc.url {
                    items.push(SourceItem {
                        key: url.clone(),
                        path: None,
                        url: Some(url.clone()),
                        content_size: 0,
                    });
                } else if let Some(ref path) = sc.path {
                    let resolved = agent_dir.join(path);
                    if resolved.is_dir() {
                        collect_source_files(&resolved, agent_dir, &mut items)?;
                    } else if resolved.is_file() {
                        let size = std::fs::metadata(&resolved)?.len();
                        let key = resolved
                            .strip_prefix(agent_dir)
                            .unwrap_or(&resolved)
                            .to_string_lossy()
                            .to_string();
                        items.push(SourceItem {
                            key,
                            path: Some(resolved),
                            url: None,
                            content_size: size,
                        });
                    }
                } else if let Some(ref glob_pattern) = sc.glob {
                    let pattern = agent_dir
                        .join(glob_pattern)
                        .to_string_lossy()
                        .to_string();
                    for entry in glob::glob(&pattern).map_err(|e| CliError::ConfigInvalid {
                        message: format!("Invalid glob: {}", e),
                    })? {
                        let path = entry.map_err(|e| CliError::Io(e.into_error()))?;
                        if path.is_file() && is_supported_curriculum_file(&path) {
                            let size = std::fs::metadata(&path)?.len();
                            let key = path
                                .strip_prefix(agent_dir)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .to_string();
                            items.push(SourceItem {
                                key,
                                path: Some(path),
                                url: None,
                                content_size: size,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(items)
}

fn collect_source_files(
    dir: &std::path::Path,
    agent_dir: &std::path::Path,
    items: &mut Vec<SourceItem>,
) -> Result<(), CliError> {
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() && is_supported_curriculum_file(entry.path()) {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let key = entry
                .path()
                .strip_prefix(agent_dir)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .to_string();
            items.push(SourceItem {
                key,
                path: Some(entry.path().to_path_buf()),
                url: None,
                content_size: size,
            });
        }
    }
    Ok(())
}

fn hash_source(source: &SourceItem) -> Result<String, CliError> {
    if let Some(ref path) = source.path {
        let contents = std::fs::read(path)?;
        let mut hasher = Sha256::new();
        hasher.update(&contents);
        Ok(hex::encode(hasher.finalize()))
    } else {
        Ok(String::new())
    }
}

pub async fn load_manifest_from_image(
    runtime: &dyn ContainerRuntime,
    image: &str,
) -> Result<serde_json::Value, CliError> {
    let temp_name = format!("pupil-manifest-{}", uuid_short());
    let id = runtime
        .run(
            image,
            &RunOptions {
                name: Some(temp_name),
                entrypoint: Some("cat".to_string()),
                command: vec!["/data/.pupil-manifest.json".to_string()],
                remove_on_exit: true,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to read manifest: {}", e),
        })?;

    let output = runtime
        .logs(&id, false, 0)
        .await
        .map_err(|e| CliError::ContainerRuntimeError {
            message: format!("Failed to read manifest logs: {}", e),
        })?;

    let _ = runtime.rm(&id, false).await;

    let manifest: serde_json::Value = serde_json::from_str(&output)?;
    Ok(manifest)
}

fn parse_build_summary(stderr: &str) -> Option<BuildSummary> {
    for line in stderr.lines().rev() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("event").and_then(|e| e.as_str()) == Some("build_complete") {
                return Some(BuildSummary {
                    memories_created: v["memories"].as_u64().unwrap_or(0),
                    input_tokens: v["input_tokens"].as_u64().unwrap_or(0),
                    output_tokens: v["output_tokens"].as_u64().unwrap_or(0),
                    input_cost: v["input_cost"].as_f64().unwrap_or(0.0),
                    output_cost: v["output_cost"].as_f64().unwrap_or(0.0),
                });
            }
        }
    }
    None
}

fn analyze_test_failures(
    test_file_path: &std::path::Path,
    test_result_json: &serde_json::Value,
) -> Result<FailureAnalysis, CliError> {
    // 1. Parse summary from test results
    let summary = &test_result_json["summary"];
    let total = summary["total"].as_u64().unwrap_or(0) as usize;
    let passed = summary["passed"].as_u64().unwrap_or(0) as usize;
    let failed = summary["failed"].as_u64().unwrap_or(0) as usize;
    let pass_rate = summary["pass_rate"].as_f64().unwrap_or(0.0);

    // 2. Load test definitions from host-side file to get source mappings
    let test_yaml = std::fs::read_to_string(test_file_path).map_err(|e| {
        CliError::ConfigInvalid {
            message: format!(
                "Cannot read test file '{}': {}",
                test_file_path.display(),
                e
            ),
        }
    })?;

    #[derive(serde::Deserialize)]
    struct TestCaseDef {
        question: String,
        #[serde(default)]
        sources: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    struct TestFileDef {
        tests: Vec<TestCaseDef>,
    }

    let test_defs: TestFileDef =
        serde_yml::from_str(&test_yaml).map_err(|e| CliError::Yaml(e))?;

    // 3. Build a question -> sources lookup.
    //    The test results JSON identifies tests by "name", which defaults to
    //    the question text when no explicit name is set (see pupil-agent main.rs).
    let source_map: HashMap<String, Vec<String>> = test_defs
        .tests
        .into_iter()
        .map(|t| (t.question.clone(), t.sources))
        .collect();

    // 4. Find failed test questions from results and collect their sources
    let mut failed_sources: Vec<String> = Vec::new();

    let empty_tests = Vec::new();
    let failed_tests = test_result_json["tests"]
        .as_array()
        .unwrap_or(&empty_tests)
        .iter()
        .filter(|t| t["passed"].as_bool() == Some(false));

    for test in failed_tests {
        // The "name" field in results falls back to the question text
        let name = test["name"].as_str().unwrap_or("");
        let question = test["question"].as_str().unwrap_or("");

        // Try matching by question first (exact match), then by name
        let sources = source_map
            .get(question)
            .or_else(|| source_map.get(name));

        if let Some(sources) = sources {
            for source in sources {
                // Normalize to curriculum/ prefix for the --source flag
                let normalized = if source.starts_with("curriculum/") {
                    source.clone()
                } else {
                    format!("curriculum/{}", source)
                };
                if !failed_sources.contains(&normalized) {
                    failed_sources.push(normalized);
                }
            }
        }
    }

    Ok(FailureAnalysis {
        failed_sources,
        pass_count: passed,
        fail_count: failed,
        total,
        pass_rate,
    })
}

fn build_remedial_prompt(analysis: &FailureAnalysis) -> String {
    let mut prompt = String::from(
        "You previously studied this curriculum but missed some details.\n\
         Re-read the following source documents carefully and look for\n\
         information you may have overlooked or stored incorrectly:\n\n",
    );

    if !analysis.failed_sources.is_empty() {
        prompt.push_str("Documents to re-read:\n");
        for source in &analysis.failed_sources {
            prompt.push_str(&format!("- {}\n", source));
        }
        prompt.push('\n');
    }

    prompt.push_str(
        "Use recall_memories to check what you already know from these documents.\n\
         Look for gaps, missing details, and incorrect information. Store any\n\
         additional insights you find. Do NOT re-store information you have\n\
         already stored -- use find_similar_memories to check for duplicates first.\n",
    );

    prompt
}

/// Execute the test suite inside the container and return the parsed JSON
/// result. The test file is bind-mounted read-only at container startup.
async fn run_self_test_in_container(
    runtime: &dyn ContainerRuntime,
    base_run_args: &[String],
    agent_name: &str,
    judge_model: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    let test_container = format!("pupil-test-{}-{}", agent_name, uuid_short());
    println!("Self-test: running tests... (watch logs: docker logs -f {})", test_container);

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        test_container,
    ];
    args.extend(base_run_args.to_vec());
    args.push(BASE_IMAGE.to_string());
    args.extend([
        "--config".to_string(), "/tmp/pupil.yaml".to_string(),
        "test".to_string(),
        "--file".to_string(), "/tmp/_pupil_test_suite.yaml".to_string(),
        "--json".to_string(),
    ]);

    if let Some(model) = judge_model {
        args.extend([
            "--judge-model".to_string(),
            model.to_string(),
        ]);
    }

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let test_output = crate::container::execute_runtime_command(
        runtime.binary_path(),
        "run",
        &refs,
    )
    .await
    .map_err(|e| CliError::ContainerRuntimeError {
        message: format!("Self-test failed: {}", e),
    })?;

    let test_result: serde_json::Value =
        serde_json::from_str(&test_output.stdout).unwrap_or_else(|_| {
            serde_json::json!({
                "summary": {
                    "total": 0,
                    "passed": 0,
                    "failed": 0,
                    "pass_rate": 0.0
                },
                "tests": []
            })
        });

    Ok(test_result)
}

/// Execute a remedial learning pass: re-read specific sources with a focus
/// prompt that guides the agent toward weak topic areas.
///
/// The remedial prompt is base64-encoded and passed via --remedial-prompt to
/// avoid shell escaping issues with docker exec.
async fn run_remedial_learning(
    runtime: &dyn ContainerRuntime,
    base_run_args: &[String],
    agent_name: &str,
    analysis: &FailureAnalysis,
) -> Result<(), CliError> {
    let remedial_prompt = build_remedial_prompt(analysis);

    use base64::Engine;
    let encoded_prompt = base64::engine::general_purpose::STANDARD
        .encode(remedial_prompt.as_bytes());

    let sources_description = if analysis.failed_sources.is_empty() {
        "all sources".to_string()
    } else {
        format!(
            "{} source(s): {}",
            analysis.failed_sources.len(),
            analysis.failed_sources.join(", ")
        )
    };

    let remedial_container = format!("pupil-remedial-{}-{}", agent_name, uuid_short());
    println!(
        "Remedial learning: re-reading {} (watch logs: docker logs -f {})",
        sources_description, remedial_container
    );

    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        remedial_container,
    ];
    args.extend(base_run_args.to_vec());
    args.push(BASE_IMAGE.to_string());
    args.extend([
        "--config".to_string(), "/tmp/pupil.yaml".to_string(),
        "learn".to_string(),
        "--remedial-prompt".to_string(), encoded_prompt,
        "--force-relearn".to_string(),
    ]);

    if !analysis.failed_sources.is_empty() {
        for source in &analysis.failed_sources {
            args.push("--source".to_string());
            args.push(source.clone());
        }
    }

    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    crate::container::execute_runtime_command(
        runtime.binary_path(),
        "run",
        &refs,
    )
    .await
    .map_err(|e| CliError::ContainerRuntimeError {
        message: format!("Remedial learning failed: {}", e),
    })?;

    println!("Remedial learning complete: re-read {}", sources_description);
    Ok(())
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

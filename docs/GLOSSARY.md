# Glossary

Alphabetical reference of domain-specific terms, components, commands, configuration fields, and architectural concepts used in the Pupil project.

## A

**Agent-as-MCP-server** - A Phase 3 feature that exposes running Pupil agents as callable MCP tools. Enables hierarchical routing where a coordinator agent invokes specialist agents as tool calls.

**AgentEntry** - A struct in the routing engine that holds an agent's name, URL, description, topics, exclusive topics, sample questions, priority, and health status.

**AgentRegistry** - The module (`router/registry.rs`) responsible for maintaining the list of known agents, their metadata, and their health status for the multi-agent router.

**Assertion types** - Categories of test expectations in `pupil test`. Includes string assertions (contains, not_contains, matches), retrieval assertions (memory_hit, memory_source, tool_called), scored assertions (llm_judge, semantic_similarity, faithfulness), and operational assertions (latency_ms, token_count).

**AssertionResult** - A struct capturing the outcome of a single test assertion: type, pass/fail, optional score, threshold, and human-readable detail.

**async-openai** - Rust crate (v0.36) used as the OpenAI-compatible fallback LLM backend. Handles Azure OpenAI, custom endpoints, and any `/v1/chat/completions` compatible service.

**Audit log** - An opt-in record of queries, retrieved memories, and agent responses written to `/data/audit/` as daily JSONL files. Supports privacy controls including regex redaction and configurable retention.

**auto strategy** - The default sync change detection strategy. Tries HTTP conditional requests (ETag/Last-Modified) first, then falls back to content hash comparison.

## B

**Base image** - The runtime container foundation, `gcr.io/distroless/static-debian12:nonroot`. Adds approximately 2MB over scratch while providing CA certificates, a non-root user (UID 65532), and timezone data.

**Build pipeline (v1)** - Container commit workflow: start a build container, exec `pupil-agent --learn`, commit the container as a new image, remove the container.

**Build pipeline (v2)** - Programmatic OCI layer append: run learning in a temp container, extract `/data/recalld`, create a tar.gz layer, append to the base image manifest via `oci-client`.

## C

**CancellationToken** - A `tokio-util` primitive used for coordinated graceful shutdown of MCP server processes on SIGINT/SIGTERM.

**CapturedToolCall** - A struct that records an intercepted tool call during test execution: tool name, arguments, result, and duration.

**ChatConfig** - Configuration passed to `LlmProvider.chat()` controlling parameters like temperature, max tokens, and model-specific settings.

**Claim decomposition** - The first step of faithfulness scoring. An LLM breaks an agent's response into atomic factual statements that can each be independently verified against retrieved context.

**Confluence API strategy** - A sync change detection strategy (`confluence_api`) that uses Confluence REST API version numbers instead of fetching full page content. Detects changes via `GET /wiki/rest/api/content/{id}?expand=version`.

**confidence_threshold** - A router configuration value (0.0-1.0) specifying the minimum routing confidence score required to route a query to an agent. Queries below this threshold go to the fallback.

**ConversationManager** - The struct managing message history, session identity, and token usage tracking. Supports serialization for session persistence at `/data/sessions/{session_id}.json`.

**Container label discovery** - A Phase 3 agent discovery mechanism where agents announce themselves via Docker container labels or Kubernetes pod annotations with the `pupil.router.*` prefix.

**ContainerRuntime** - A trait abstracting over Docker and Podman CLIs. Provides methods for run, exec, commit, build, push, pull, rm, cp, and logs. Both implementations shell out via `std::process::Command`.

**Content hashing** - SHA-256 hashing of raw file bytes (or URL response body) used for incremental build tracking and sync change detection when HTTP conditional requests are unavailable.

**Context windowing** - The process of splitting large documents at heading boundaries into sections that fit the LLM's context window, preserving document title and heading path for context.

**CostTracker** - A struct that records cumulative and per-source (build) or per-session (runtime) token usage, computes estimated cost from the model pricing table, and enforces budget limits.

**Crawl-Delay** - A robots.txt directive specifying minimum delay between requests. Pupil respects this value and uses it to override `request_delay_ms` for that host when the robots.txt value is higher.

**curriculum/** - A directory within an agent project containing the source material the agent should learn. Supports markdown, plain text, PDF, HTML/URLs, and additional formats in later phases.

## D

**DashMap** - A concurrent HashMap from the `dashmap` crate (v6) used for session storage in HTTP mode and session affinity in the router.

**Debouncing** - Coalescing multiple filesystem events (caused by non-atomic editor saves) into a single logical event. Uses a 500ms timeout via `notify-debouncer-full`.

**decay** - A `curriculum` config field controlling the recalld memory decay rate multiplier. `0.0` disables decay entirely, meaning learned memories never weaken over time.

**Dev container** - A long-lived container used by `pupil watch` with the curriculum directory bind-mounted from the host. Named `pupil-dev-{agent-name}`, separate from production images.

**distroless** - Google's minimal container base images. Pupil uses `distroless/static-debian12:nonroot` to keep the runtime image small and secure.

**DynService** - The type-erased MCP service type produced by rmcp's `into_dyn()` method. Enables storage of heterogeneous MCP server transports in a single HashMap.

## E

**embeddinggemma** - The default Ollama embedding model. 300M parameters, 768 dimensions, 2K token context. Produces higher quality embeddings than alternatives and has strong multilingual support.

**Embedding similarity (routing)** - A routing strategy that embeds the user query and compares it via cosine similarity against pre-computed embeddings of agent descriptions and sample questions.

**Error codes** - Structured error identifiers used by `miette` diagnostics. E001 (config), E002 (environment), E003 (build), E004 (runtime), E005 (registry), E006 (LLM).

**exclusive_topics** - A routing metadata field listing topics that only one specific agent should handle. If a query matches an exclusive topic, that agent wins immediately regardless of other scores.

**Extractor** - A common trait implemented by each curriculum format parser (markdown, PDF, HTML, text). Lives behind Cargo feature flags so Phase 2/3 formats do not affect core binary size.

## F

**Faithfulness** - A scored test assertion measuring whether the agent's response is grounded in retrieved memories rather than hallucinated. Score = (supported claims) / (total claims). Adapted from RAGAS and deepeval.

**fallback_model** - A `pupil.yaml` config field specifying a cheaper model to degrade to when the daily runtime budget is exceeded with `on_budget_exceeded: degrade`.

**Fan-out queries** - A Phase 3 routing feature where the router sends a query to multiple agents in parallel, collects responses, and uses an LLM to synthesize a unified answer.

**First-run wizard** - An interactive setup flow triggered on first invocation when `~/.config/pupil/config.yaml` does not exist. Uses `dialoguer` to detect the container runtime, select the LLM provider, enter API keys, and validate connectivity. Bypass with `PUPIL_SKIP_SETUP=1`.

## G

**genai** - The primary multi-provider LLM client crate (v0.6). Supports 25+ providers through native protocol adapters. Covers Anthropic, OpenAI, Google Gemini, Ollama, Bedrock, and Vertex.

**GenaiProvider** - The default implementation of `LlmProvider` using the `genai` crate. Handles most providers through native protocol adapters.

**Global configuration** - User-level defaults stored at `~/.config/pupil/config.yaml`. Includes `default_provider`, `default_model`, `container_runtime`, and `default_registry`.

## H

**Hierarchical routing** - A Phase 3 multi-agent pattern where a general-purpose coordinator agent receives all queries and invokes specialist agents as MCP tools, synthesizing their responses.

**Hot reload** - The mechanism in `pupil watch` that re-learns changed files in-place inside a running dev container via `docker exec`, without rebuilding the image or restarting the container.

**Hybrid strategy (routing)** - The recommended routing strategy. Runs keyword matching first, then embedding similarity, then LLM classification, stopping at whichever tier produces a confident match.

## I

**Incremental builds** - The build optimization that skips re-learning unchanged curriculum items. Uses SHA-256 content hashes stored in `.pupil-manifest.json` to detect changes. Only new or changed sources are processed.

**in_place** - A sync `on_change` mode (default) where changed URL sources are re-learned directly inside a running container without rebuilding the image. Fast but does not update the distributable image.

## J

**judge_model** - A test config field specifying which LLM model to use for `llm_judge` assertions. Defaults to the agent's own model. Using a cheaper model (claude-haiku-4) keeps test costs low.

## K

**Keyword/topic matching** - The cheapest routing strategy. Tokenizes the query and counts substring matches against each agent's configured `topics` list. Zero cost, sub-millisecond latency.

## L

**learn feature flag** - A Cargo feature flag on `pupil-agent` that gates learning-related dependencies (pulldown-cmark, pdf-extract, scraper, readability-rust).

**Learning pipeline** - The full agentic process triggered by `pupil build`. A learning agent reads curriculum sources, comprehends the material, and decides what knowledge to extract and how many memories to create. Not a mechanical chunking system.

**learning_model** - A `pupil.yaml` config field specifying the LLM model used at build time for curriculum learning. Optional; defaults to the runtime `model` field. Allows using a more capable model for learning and a cheaper one for runtime.

**learning_profile** - A named set of learning guidelines tuned for a specific content type. Built-in profiles: `general`, `reference`, `procedural`, `conceptual`, `faq`, `policy`, `code`. Mutually exclusive with `learning_prompt`.

**learning_prompt** - A `pupil.yaml` per-source config field for custom learning guidelines text. Replaces only the Guidelines section of the learning prompt; base instructions are always prepended. Mutually exclusive with `learning_profile`.

**Live Source Sync** - The feature that periodically re-crawls URL-based curriculum sources and updates the agent's knowledge in-place without a full manual rebuild. See `pupil sync`.

**LLM classifier (routing)** - A routing strategy that asks an LLM to select the best agent given a query and the list of agent descriptions. Most accurate but slowest and most expensive.

**LlmProvider** - The trait abstracting over LLM backends. Defines `chat()` and `chat_stream()` methods. Two implementations: `GenaiProvider` (default) and `OpenAiCompatProvider`.

**llm_judge** - A scored test assertion that sends the question, agent response, and evaluation criteria to a judge LLM. Returns a 0.0-1.0 score. Approximately 80% agreement with human evaluators.

## M

**max_cost_usd** - A `build` config field setting the maximum allowed LLM cost for a single build. When exceeded, the configured `on_budget_exceeded` action triggers (abort, confirm, or warn).

**max_iterations** - An agent config field (default: 50) capping the number of ReAct loop iterations per query. Prevents infinite tool-call loops.

**max_tokens_per_query** - A runtime config field capping total tokens (input + output) per query across all ReAct iterations. Prevents runaway queries.

**McpManager** - The central abstraction for managing multiple MCP server connections. Maintains a HashMap of server name to DynService and a tool_index mapping tool names to server names. Handles tool name collision resolution by prefixing.

**McpServerConfig** - The parsed configuration for a single MCP server entry from the `mcp_servers` section of `pupil.yaml`. Includes command, args, env, and the `required` flag.

**memory_hit** - A retrieval test assertion checking whether the agent called `recall_memories` and received at least one result. `memory_hit: false` asserts the agent correctly found nothing.

**memory_source** - A retrieval test assertion checking that at least one retrieved memory has a tag matching `source/<filename>`. Validates the agent pulled knowledge from the expected curriculum source.

**metrics** - A Rust facade crate (v0.24) providing `counter!`, `gauge!`, and `histogram!` macros. Used with `metrics-exporter-prometheus` to expose a `/metrics` endpoint.

## N

**namespace** - A recalld memory partition specified in the `curriculum` section of `pupil.yaml`. Isolates the agent's learned knowledge from other data in the same recalld instance.

**Notion API strategy** - A sync change detection strategy (`notion_api`) that checks `last_edited_time` on Notion pages. Enforces Notion's 3 requests/second rate limit automatically.

**notify** - A cross-platform filesystem watcher crate (v8) used by `pupil watch`. Uses FSEvents on macOS, inotify on Linux, ReadDirectoryChanges on Windows.

**notify-debouncer-full** - A companion crate to `notify` (v0.7) providing debounced event batching with file ID tracking for rename detection. Used in `pupil watch`.

## O

**OCI** - Open Container Initiative. The standard format for container images. Pupil uses OCI-compatible images and registries for agent distribution.

**oci-client** - A Rust crate (v0.13) for OCI registry push/pull/auth operations. Used in the v2 build pipeline for programmatic image construction.

**ocipkg** - A Rust crate (v0.3) for OCI archive tar format. Used by `pupil export` and `pupil import` for offline distribution.

**Ollama sidecar** - A Docker Compose service running Ollama alongside the agent container on an internal network. Provides local embedding generation and optional local LLM inference. Not baked into the agent image, keeping it under 50MB.

**on_budget_exceeded** - A config field (used in both `build` and `runtime` sections) specifying what happens when a cost budget is exceeded. Values: `abort`, `confirm`, `warn`, or `degrade` (runtime only).

**on_change** - A sync config field controlling behavior when changed content is detected. Values: `in_place` (update memories live), `rebuild` (trigger full `pupil build`), `notify` (report changes only).

**OpenAiCompatProvider** - The fallback implementation of `LlmProvider` using `async-openai`. Handles Azure OpenAI, custom endpoints, and any `/v1/chat/completions` compatible service.

## P

**Parallel tool execution** - When the LLM returns multiple `tool_use` blocks in a single response, all are executed concurrently via `tokio::spawn` and results returned together. Tool timeouts default to 30 seconds.

**Peer suggestions** - System prompt content injected by the router listing other available agents. Enables agents to suggest that users ask a different specialist when a question is outside their domain.

**Pricing table** - A compiled-in `HashMap<&str, (f64, f64)>` in `llm/pricing.rs` mapping model names to (input price, output price) per million tokens. Users can override pricing in `pupil.yaml` for custom models.

**Profiles directory** - `crates/pupil-agent/src/learn/profiles/` containing one file per built-in learning profile. Included at compile time via `include_str!()`.

**PUPIL_CONTAINER_RUNTIME** - Environment variable to override container runtime detection. Takes precedence over `which docker` / `which podman` / socket probing.

**PUPIL_LOG_FORMAT** - Environment variable controlling log output format. `human` for ANSI-colored readable output (default interactive), `json` for structured JSON (default detached/HTTP mode).

**PUPIL_SKIP_SETUP** - Environment variable that bypasses the first-run wizard when set to `1`. Used in CI environments.

**pupil-agent** - The in-container binary that serves as the agent runtime. Reads `pupil.yaml`, starts MCP servers, connects to an LLM provider, and runs the ReAct conversation loop. Also handles curriculum learning (`--learn`), testing (`--test`), and single-source operations (`--learn --source`, `--forget-source`).

**pupil-cli** - The host-side binary that manages the full agent lifecycle. Wraps docker/podman so users never write container commands directly. Provides all `pupil` commands including create, teach, build, run, push, pull, test, inspect, watch, sync, and router.

**pupil-router** - The multi-agent routing component. In Phase 1-2, embedded in `pupil-cli` as a library module. May move to its own crate in Phase 3. Receives queries, classifies intent, and proxies to the appropriate agent.

**pupil-router.yaml** - The static configuration file for the multi-agent router. Lists agents with their URLs, descriptions, topics, and sample questions.

**pupil.yaml** - The agent definition file. Specifies agent name, description, model, system prompt, MCP servers, curriculum sources, learning profiles, cost budgets, test config, routing metadata, and runtime settings.

## R

**ReAct conversation loop** - The standard agent pattern: receive input, call the LLM with tools, execute tool calls, feed results back, repeat until the model stops calling tools. Implemented in `agent.rs`.

**readability-rust** - A Rust port of Mozilla Readability used for article extraction from web pages during source preparation. Cleans HTML to markdown.

**recalld** - An MCP server providing the memory and knowledge layer. Runs inside the agent container. Stores learned knowledge in SQLite with hybrid search (vector + FTS5). Supports spaced repetition decay, memory relationships, and the `supersedes` pattern.

**Remedial learning** - An optional build-time process triggered when self-test scores fall below the threshold. The learning agent re-reads sources tagged with failed topics, guided to focus deeper. Never sees the test questions or expected answers.

**request_delay_ms** - A sync config field (default: 500ms) setting the minimum delay between HTTP requests to the same host. A politeness control for external URLs.

**respect_robots_txt** - A sync config field (default: true) controlling whether to check robots.txt before crawling external URLs. Internal URLs with auth bypass this check.

**rmcp** - The official Rust MCP SDK (v1.8, 4.7M+ downloads). Used for spawning and communicating with MCP server child processes. Provides `TokioChildProcess` transport and `into_dyn()` for type erasure.

**Rolling context strategy** - During learning, after every N sections (or at 80% of context window), the learning agent summarizes what it has learned, the conversation resets to system prompt plus summary, and learning continues. Keeps context bounded while preserving continuity.

**RoutingDecision** - A struct holding the result of the routing engine: selected agent name, confidence score, strategy tier that matched, optional reasoning, and alternative candidates with their scores.

**RoutingEngine** - The core routing component that evaluates queries against registered agents. Supports keyword, embedding, LLM classifier, and hybrid strategies. Lives in `pupil-cli/src/router/mod.rs`.

## S

**Scored assertions** - Test assertions that produce a 0.0-1.0 score instead of binary pass/fail. Includes `llm_judge`, `semantic_similarity`, and `faithfulness`. Pass when the score meets or exceeds the configured threshold (default: 0.8).

**SecretString** - A type from the `secrecy` crate (v0.10) wrapping API keys with zero-on-drop semantics and automatic redaction in log output.

**Self-test learning cycle** - An optional build-time loop (`build.self_test` in pupil.yaml) where the agent learns, runs a test suite, analyzes failures by topic, performs remedial learning on weak areas, and retests. The test content is never visible to the learning agent.

**semantic_similarity** - A scored test assertion computing cosine similarity between the response embedding and a reference text embedding. Requires an embedding provider.

**Session affinity** - Router behavior that routes subsequent messages in the same conversation to the same agent. Prevents context loss on follow-up questions. Configurable TTL and explicit break via `X-Pupil-Reroute: true` header.

**Sitemap strategy** - A sync change detection strategy that fetches sitemap.xml and checks `<lastmod>` dates for known URLs. Used as a fast pre-filter when syncing many URLs from the same site.

**Source preparation** - The mechanical text extraction step before the learning agent processes material. Parses format-specific content (markdown headings, PDF text streams, HTML articles) into clean text.

**SourceConfig** - The parsed configuration for a single curriculum source entry. Includes path/url/glob, optional learning_profile or learning_prompt, namespace override, extra tags, and decay override.

**Spaced repetition decay** - recalld's memory decay mechanism inspired by spaced repetition systems. Memories weaken over time unless reinforced. Controlled by the `decay` config field.

**Stdout discipline** - The requirement that all agent logging goes to stderr. Stdout is the JSON-RPC transport for stdio MCP servers; any stdout pollution breaks the MCP protocol.

**supersedes** - A recalld field on `store_memory` that points to an older memory ID being replaced. The old memory is deprioritized in search results. Used when updated content contradicts existing knowledge.

**Sync state** - Per-URL tracking data stored in `.pupil-manifest.json` including last_checked, last_changed, etag, last_modified, content_hash, check_count, change_count, consecutive_errors, and last_error. Supports exponential error backoff.

**System prompt builder** - The module (`prompt.rs`) that assembles the agent's system prompt from layers: agent identity, core instructions from pupil.yaml, memory usage guidelines, available tools, and response guidelines.

## T

**Template variables** - Placeholders in learning prompts using `{variable_name}` syntax. Available variables: `{source_file}`, `{source_path}`, `{agent_name}`, `{namespace}`, `{heading_path}`, `{source_type}`. Simple string substitution, not a template engine.

**TestResult** - A struct capturing the outcome of a single test case: name, question, response, assertion results, latency, token counts, captured tool calls, retries used, and overall pass/fail.

**tests.yaml** - The default test file for `pupil test`. Contains a `config` section (temperature, retries, timeout, judge_model, threshold) and a `tests` list with questions and expected assertions.

**texting_robots** - A Rust crate (v0.2) for robots.txt parsing. Battle-tested against 34M+ real-world files. Used by sync to check crawl permissions before fetching external URLs.

**TokenUsage** - A struct tracking `input_tokens` and `output_tokens` separately. Input and output are tracked independently because output tokens cost 3-5x more across most providers.

**tool_index** - A HashMap inside McpManager mapping tool names to server names. Used to route tool calls to the correct MCP server.

## U

**Usage log** - Daily JSONL files at `/data/usage/` recording per-query token usage, cost, tool calls, and latency. Read by `pupil status` for daily/monthly aggregate reporting.

## V

**Volume mount (`/data`)** - The only writable persistent mount in the agent container. Contains recalld's SQLite database, session state files, usage logs, and audit logs. On first run, baked-in data from the image is copied to the volume.

## W

**Webhook strategy** - A push-based sync change detection strategy. The sync daemon listens for incoming webhooks from source platforms (Confluence, GitHub). Combined with periodic polling as a consistency safety net.

**webhook_listen** - A sync config field (e.g., `"0.0.0.0:9090"`) specifying the listen address for incoming webhook notifications.

## X

**X-Pupil-Routed-To** - An HTTP response header added by the router indicating which agent handled the query.

**X-Pupil-Reroute** - An HTTP request header that, when set to `true`, explicitly breaks session affinity and forces the router to re-evaluate which agent should handle the query.

## CLI Commands

**pupil build** - Validates environment, starts a build container, runs `pupil-agent --learn` to learn the curriculum, and commits the container as a new image. Supports `--dry-run`, `--no-cache`, and `--no-confirm`.

**pupil commit** - Snapshots the current runtime volume state (including runtime-learned knowledge) into a new image for distribution. The bridge between runtime learning and distributable knowledge.

**pupil completions** - Generates shell completion scripts via `clap_complete` for bash, zsh, fish, and PowerShell.

**pupil config** - Gets, sets, or lists global configuration values from `~/.config/pupil/config.yaml`.

**pupil create** - Scaffolds a new agent directory with `pupil.yaml` and `curriculum/`. Supports `--template` and `--model` flags.

**pupil doctor** - Validates the environment: checks for container runtime, API keys, Ollama availability, and other prerequisites.

**pupil export** - Saves an agent image as an OCI archive tar file for offline distribution.

**pupil import** - Loads an OCI archive tar file and registers the agent locally.

**pupil inspect** - Views, searches, and analyzes learned memories. Subcommands: `list`, `show`, `search`, `stats`, `quality`, `graph`, `diff`. Does not require an LLM API key (runs recalld directly).

**pupil list** - Lists locally registered agents in a formatted table.

**pupil logs** - Shows logs from a running agent container. Supports `--follow` and `--tail`.

**pupil pull** - Pulls an agent image from an OCI-compatible registry and registers it locally.

**pupil push** - Pushes an agent image to an OCI-compatible registry.

**pupil router** - Manages the multi-agent routing server. Subcommands: `start`, `stop`, `status`, `test`, `add`, `remove`, `generate-config`.

**pupil run** - Starts an agent container and enters interactive chat (stdin/stdout) or HTTP server mode (`--port`). Supports `--detach` for background operation and `--with-ollama` for the Ollama sidecar.

**pupil status** - Shows agent information including build details, memory count, runtime usage (queries, tokens, cost), sync status, and latency metrics.

**pupil sync** - Checks URL-based curriculum sources for changes and re-learns any that changed. Supports `--force`, `--dry-run`, `--source`, `--json`, and `--daemon` for continuous scheduled sync.

**pupil teach** - Adds content to the curriculum by copying files into `curriculum/` or recording URLs in the config. Supports `--url`, `--recursive`, `--glob`, and `--dry-run`.

**pupil test** - Validates that a built agent learned its curriculum by running test cases from a YAML file. Supports string, retrieval, scored, and operational assertions. Outputs human-readable, JSON, or JUnit XML results. Supports `--generate` for LLM-powered test generation from curriculum.

**pupil watch** - Development mode with filesystem watching. Keeps a long-lived dev container with bind-mounted curriculum, re-learns changed files in-place via `docker exec`, and optionally runs tests after each re-learn.

## Files

**.pupil-manifest.json** - Stored at `/data/.pupil-manifest.json` inside the container. Tracks content hashes, memory IDs per source, learning timestamps, build history (tokens, cost), sync state for URL sources, and embedding configuration. The source of truth for incremental builds.

**.pupil-test-baseline.json** - A Phase 2 snapshot of test results used for regression detection. Subsequent runs with `--compare-baseline` distinguish newly broken tests from known failures.

**pupil-router.yaml** - Static configuration for the multi-agent router. Defines listen address, routing strategy, fallback behavior, confidence thresholds, and the agent registry.

**pupil.yaml** - The agent definition file at the root of each agent directory. Defines name, description, model, system_prompt, mcp_servers, curriculum, build settings, runtime settings, routing metadata, audit config, and pricing overrides.

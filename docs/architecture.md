# Architecture

This document describes how Pupil is structured internally: the two Rust crates, the container strategy, how the LLM learns curriculum, and how knowledge is persisted.

## 1. Overview

Pupil is a Cargo workspace (`Cargo.toml` at the repo root) with two crates:

### `pupil-cli` (host-side CLI)

The `pupil-cli` crate is the command-line tool you install on your machine. It handles:

- Agent scaffolding (`pupil create`)
- Curriculum management (`pupil teach`)
- Container orchestration during build (`pupil build`)
- Running agent containers (`pupil run`)
- Registry operations (`pupil push`, `pupil pull`)
- Test orchestration (`pupil test`)
- Knowledge inspection (`pupil inspect`)
- File watching and URL sync (`pupil watch`, `pupil sync`)
- Multi-agent routing (`pupil router`)
- Global configuration management (`pupil config`)

The CLI never performs learning directly. It starts containers and delegates learning to the `pupil-agent` binary running inside them.

### `pupil-agent` (in-container binary)

The `pupil-agent` crate compiles into a statically linked binary (musl target) that runs inside the container. It handles:

- **Learning mode** (`learn` subcommand): reads curriculum files, calls the LLM to comprehend them, and stores memories via recalld's MCP interface. Gated behind the `learn` Cargo feature flag.
- **Interactive mode** (default, no subcommand): reads user messages from stdin, queries recalld for relevant memories, calls the LLM, and writes responses to stdout.
- **HTTP server mode** (`--port N`): exposes an OpenAI-compatible `/v1/chat/completions` API with session management, health checks, and SSE streaming.
- **Test mode** (`test` subcommand): runs test cases from a `tests.yaml` file, evaluates assertions (including LLM judge), and outputs JSON results.
- **Forget mode** (`forget-source` subcommand): deletes all memories associated with a specific source.
- **Idle mode** (`idle` subcommand): blocks until SIGTERM. Used as a keep-alive process for build containers on distroless images that have no shell or `sleep`.

The workspace pins shared dependencies at the workspace level (`[workspace.dependencies]` in the root `Cargo.toml`). Both crates use the 2024 edition (Rust 1.85+) and build with aggressive size optimizations in release mode: `opt-level = "z"`, full LTO, single codegen unit, abort-on-panic, and symbol stripping.

On musl targets, `pupil-agent` uses jemalloc (`tikv-jemallocator`) as the global allocator.

## 2. Container Strategy

### Dockerfile stages

The container uses a multi-stage Dockerfile (`container/Dockerfile`) with three stages:

**Stage 0 (recalld-builder)**: Clones and compiles [recalld](https://github.com/calebevans/recalld) at a pinned version (currently v0.1.1) from source using `rust:1-alpine`. The `TARGETARCH` build argument (set by `docker buildx`) is mapped to the appropriate Rust target triple (`x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`) for static linking.

**Stage 1 (builder)**: Copies the full Pupil workspace into the build context and compiles `pupil-agent` using the same Alpine/musl strategy. Only the `pupil-agent` package is built (`-p pupil-agent`).

**Stage 2 (runtime)**: Uses `gcr.io/distroless/static-debian12:nonroot` as the final base. This image provides CA certificates (for HTTPS calls to LLM APIs), timezone data, and a non-root user (UID 65532). It contains no libc, shell, or userland tools. Total base size is approximately 2 MB.

Only two binaries are copied into the final image: `pupil-agent` and `recalld`. The final image is approximately 24 MB.

### Volume layout

The container declares a single volume at `/data`, which is the only writable directory at runtime. It holds:

| Path | Contents |
|---|---|
| `/data/recalld/` | recalld SQLite database and WAL files (the learned knowledge) |
| `/data/sessions/` | Conversation session state, stored as `{uuid}.json` files |
| `/data/usage/` | Daily JSONL usage logs |
| `/data/audit/` | Daily JSONL audit logs (opt-in) |
| `/data/.pupil-manifest.json` | Build manifest tracking which sources were learned, their content hashes, and memory IDs |

Curriculum files are bind-mounted read-only at `/curriculum` during build. The agent config (`pupil.yaml`) is bind-mounted at `/tmp/pupil.yaml` by `pupil build`, which passes `--config /tmp/pupil.yaml` to override the agent binary's default config path of `/agent/pupil.yaml` (configurable via the `PUPIL_CONFIG` env var or `--config` flag).

### Image snapshotting via `docker commit`

After the learning phase completes, `pupil build` calls `docker commit` (via `ContainerRuntime::commit()` in `crates/pupil-cli/src/container/docker.rs`) to snapshot the running container, including the populated `/data` volume, into a new OCI image. This is how learned knowledge becomes part of the image itself. The committed image is tagged (e.g., `pupil-myagent:latest`) and can be pushed to any OCI registry with `pupil push`.

The `pupil commit` command (`crates/pupil-cli/src/commands/commit.rs`) provides the same operation for running containers, allowing you to snapshot knowledge that the agent has learned at runtime (e.g., from user interactions).

### Runtime security

When running an agent with `pupil run`, the container is started with several hardening measures via `RunOptions` (`crates/pupil-cli/src/container/mod.rs`):

- `--read-only` filesystem (only `/data` is writable via the volume, and `/tmp` via tmpfs)
- `--cap-drop=ALL` (all Linux capabilities are dropped)
- `--security-opt no-new-privileges:true`
- tmpfs mount on `/tmp`

### Container runtime detection

The `container::detect()` function (`crates/pupil-cli/src/container/mod.rs`) checks for available container runtimes in the following order:

1. If `PUPIL_CONTAINER_RUNTIME` is set, use that runtime exclusively.
2. Check for `docker` in PATH, verifying the Docker socket is accessible.
3. Check for `podman` in PATH.
4. Probe known socket paths as a fallback.

Both Docker and Podman implement the `ContainerRuntime` trait, which defines `run`, `exec`, `exec_streaming`, `commit`, `build`, `push`, `pull`, `rm`, `cp`, `cp_to`, and `logs` operations.

## 3. LLM Backend

### The `LlmProvider` trait

All agent code interacts with LLMs through the `LlmProvider` trait (`crates/pupil-agent/src/llm/mod.rs`). The trait requires two async methods:

- `chat()`: Send messages with optional tool definitions, receive a `ChatResponse` containing text content, tool calls, token usage, and a stop reason.
- `chat_stream()`: Same interface but returns a `Stream` of `StreamChunk` items (text deltas, tool call starts/deltas, and a final done event with usage).

Messages are represented as a `Message` enum with four variants: `System`, `User`, `Assistant` (with optional tool calls), and `ToolResult`.

### Model resolution

The `resolve_provider()` function (`crates/pupil-agent/src/llm/provider.rs`) parses a model identifier string and returns the appropriate `LlmProvider` implementation. The parser recognizes these formats:

| Format | Provider | Example |
|---|---|---|
| `claude-*` (bare) | Anthropic | `claude-sonnet-4-6` |
| `anthropic/<model>` | Anthropic | `anthropic/claude-haiku-4` |
| `gpt-*` (bare) | OpenAI | `gpt-4o` |
| `openai/<model>` | OpenAI | `openai/gpt-4o-mini` |
| `gemini-*` (bare) | Google | `gemini-2.5-flash` |
| `google/<model>` | Google | `google/gemini-2.5-pro` |
| `ollama/<model>` or `ollama:<model>` | Ollama | `ollama/llama3` |
| `bedrock/<model>` | AWS Bedrock | `bedrock/anthropic.claude-v2` |
| `vertex/<model>` | Vertex AI | `vertex/gemini-2.5-pro` |
| `azure/<model>` | Azure OpenAI | `azure/gpt-4o` |
| `openai-compat:<base_url>/<model>` | Custom endpoint | `openai-compat:https://api.example.com/v1/my-model` |

API keys are resolved from environment variables that correspond to the provider:

| Provider | Environment variable |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Google (Gemini) | `GOOGLE_API_KEY` |
| Vertex AI | `GOOGLE_APPLICATION_CREDENTIALS` |
| Azure OpenAI | `AZURE_OPENAI_API_KEY` |
| Ollama | (none required) |
| Bedrock | (uses AWS credential chain) |
| OpenAI-compatible | `OPENAI_API_KEY` |

### GenaiProvider (primary backend)

The `GenaiProvider` (`crates/pupil-agent/src/llm/genai_provider.rs`) wraps the `genai` crate, which provides a unified interface across 25+ LLM providers. It handles:

- Converting Pupil's `Message` enum to genai's `ChatMessage` format
- Converting `ToolDefinition` to genai's `Tool` format
- Mapping genai's `StopReason` variants back to Pupil's `StopReason` enum
- Extracting token usage from genai's `Usage` type
- Streaming via `exec_chat_stream`, where genai delivers complete tool calls per chunk rather than partial deltas

For model names, the provider translates Pupil's format to genai's internal format. For example, Ollama models are prefixed with `ollama::`, Bedrock with `bedrock::`, and Vertex with `vertex::`.

### OpenAiCompatProvider (fallback backend)

The `OpenAiCompatProvider` (`crates/pupil-agent/src/llm/openai_compat.rs`) uses the `async-openai` crate for two cases:

1. **Azure OpenAI**: Configured via `AZURE_OPENAI_API_KEY`, `AZURE_OPENAI_ENDPOINT`, and optional `AZURE_OPENAI_API_VERSION`.
2. **Custom OpenAI-compatible endpoints**: Any endpoint that implements the `/v1/chat/completions` API.

The provider handles message format conversion between Pupil's types and the OpenAI API types, including tool call serialization/deserialization.

### MALFORMED_FUNCTION_CALL retry logic

Gemini models occasionally return a `MALFORMED_FUNCTION_CALL` finish reason instead of valid tool call JSON. Both the agent runtime loop (`Agent::handle_turn()` in `crates/pupil-agent/src/agent/mod.rs`) and the learning loop (`learn_source()` in `crates/pupil-agent/src/learn/learner.rs`) handle this with a retry mechanism:

1. The `StopReason::is_malformed_function_call()` method checks for this specific stop reason.
2. When detected, the broken response is NOT pushed to the conversation (it would corrupt the context).
3. A user message is injected: "Your previous tool call was malformed. Please try again with valid JSON."
4. The retry counter increments. After `MAX_MALFORMED_RETRIES` (3) consecutive failures, the turn or section is abandoned.
5. A successful iteration resets the retry counter to zero.

## 4. MCP Integration

### McpManager

The `McpManager` (`crates/pupil-agent/src/mcp/mod.rs`) manages the lifecycle of all MCP servers. It is built via `McpManager::start_all()`, which takes a map of server configurations and proceeds in five phases:

**Phase 1: Validate configs.** Check for empty commands, missing environment variables, and invalid server names. Validation errors on required servers are fatal; errors on optional servers are logged and skipped.

**Phase 2: Start servers and discover tools.** For each configured server, spawn the process (using `process_wrap` for process group management), establish the MCP connection via `rmcp`'s stdio transport, and call `list_all_tools()` to discover available tools. Required servers that fail to start or complete the MCP handshake cause the entire `start_all()` to return an error. Optional servers that fail are logged and skipped.

**Phase 3: Detect tool name collisions.** If two servers expose a tool with the same name, both tools are prefixed with their server name (e.g., `server_a.echo` and `server_b.echo`). This prevents ambiguity during tool routing.

**Phase 4: Build the tool index.** A `HashMap<String, String>` maps each tool's display name to its owning server name. A `Vec<ToolDefinition>` holds all registered tools with their schemas. Each `ToolDefinition` stores the display name (possibly prefixed), the original name (used when calling the actual MCP server), an optional description, and the JSON input schema.

**Phase 5: Log summary.** Report server count, tool count, and collision count.

### Tool routing

When `call_tool()` is invoked with a tool name:

1. Look up the name in the `tool_index`. If found, route directly.
2. If not found, pass the name through the normalization pipeline (see below). If normalization succeeds, log a warning and route to the resolved name.
3. If normalization fails, return an `UnknownTool` error. The error includes a suggestion from `closest_tool_name()` if one is close enough.
4. Strip the server prefix (if any) from the resolved name to get the original name.
5. Call the MCP server via `rmcp`'s `call_tool()` method.

Tool calls from the ReAct loop are executed in parallel using `tokio::spawn`, with a configurable timeout (`tool_timeout` from `pupil.yaml`). Timed-out or failed tool calls return error results that are fed back to the LLM.

### Tool name normalization

The normalization module (`crates/pupil-agent/src/mcp/normalize.rs`) handles LLM-hallucinated tool names through a pipeline of five strategies, tried in order:

1. **Case-insensitive exact match**: `Store_Memory` resolves to `store_memory`.
2. **CamelCase/PascalCase to snake_case conversion**: `StoreMemory` resolves to `store_memory`. Handles consecutive uppercase runs (e.g., `getHTTPResponse` becomes `get_http_response`), dots from server-prefixed names, and hyphens.
3. **Doubled word stripping**: `store_memories_memories` resolves to `store_memories`. Handles server-prefixed names by splitting on the dot and processing only the tool part.
4. **Prefix match**: If the input is a prefix of exactly one registered tool (minimum 4 characters), resolve to that tool. Ambiguous prefix matches (input matches multiple tools) return no result.
5. **Levenshtein distance**: Find the closest registered name within a maximum of 3 edits AND less than 40% of the target name's length. If two tools are equally close (ambiguous), return no result.

The `resolve_tool_name()` function returns a `NormalizedTool` with both the resolved name and the strategy that matched, which is logged for observability. If all strategies fail, `closest_tool_name()` provides a suggestion for the error message.

### Health checking

`McpManager::health_check()` calls `list_all_tools()` on each active server with a 10-second timeout. The `spawn_health_check_task()` function runs this check periodically on a configurable interval, logging results at debug level (pass) or warn level (failure).

## 5. Learning Pipeline

The learning pipeline is the process by which an agent reads curriculum and stores knowledge as memories. The LLM reads sections of content, comprehends them, and autonomously decides what to store, what to search for, and how to link related concepts.

### Source resolution

Source resolution (`crates/pupil-agent/src/learn/source.rs`) transforms curriculum entries from `pupil.yaml` into `ResolvedSource` structs. Supported source types:

- **Local files**: Markdown (`.md`, `.mdx`, `.markdown`), PDF, HTML, and plain text. Detected from file extension.
- **Directories**: Recursively walked, skipping hidden files and editor swap files.
- **URLs**: Anything starting with `http://` or `https://`.
- **Globs**: Patterns with `*`, `?`, or `[` are expanded against the curriculum directory. Supports `**` for recursive matching.

Each resolved source carries a `source_key` (its relative path or URL), a `learning_profile`, an optional custom `learning_prompt`, a `namespace`, and extra tags.

### Content sectioning

The reader module (`crates/pupil-agent/src/learn/reader.rs`) splits extracted content into `ReadingSection` structs that the LLM processes one at a time. The default maximum section size is 24,000 characters (`DEFAULT_MAX_SECTION_CHARS`).

Splitting follows document structure:

1. If the content fits in a single section, return it as-is.
2. Otherwise, split at heading boundaries from the extracted content's heading list.
3. If a segment between headings exceeds the maximum, split further at paragraph breaks (`\n\n`).
4. If a segment has no paragraph breaks, hard-split at the character limit.

Each section tracks its `document_title`, `heading_path` (e.g., "Auth > OAuth2"), `section_number`, `total_sections`, and an `is_summary_checkpoint` flag.

### Summary checkpoints

Every 5 sections (`SUMMARY_CHECKPOINT_INTERVAL`), the section is marked as a summary checkpoint. At these points, the learning loop asks the LLM to summarize what it has learned so far. The conversation is then reset to just the system prompt plus a carry-forward summary message. This prevents the conversation context from growing indefinitely during large documents.

### The agentic learning loop

The core learning function is `learn_source()` in `crates/pupil-agent/src/learn/learner.rs`. The flow for each source:

1. Initialize a `LearningConversation` with the learning system prompt.
2. For each `ReadingSection`:
   a. Send the section text to the LLM as a user message, with instructions to read carefully, extract key knowledge, store it using `store_memory`, check for related existing knowledge with `recall_memories`, and verify uniqueness with `find_similar_memories`.
   b. Enter a ReAct loop (max 50 iterations per section):
      - Call the LLM with the conversation and available tools.
      - Handle `MALFORMED_FUNCTION_CALL` with the retry mechanism.
      - If the LLM returns tool calls, execute them sequentially (one at a time) and feed results back. This differs from the runtime agent loop (section 6), which executes tool calls in parallel.
      - The `store_memory` tool call is augmented: if the LLM omits the `namespace` parameter, the learner injects it automatically.
      - Memory IDs from `store_memory` results are collected for the manifest.
      - When the LLM returns `EndTurn` with no tool calls, the section is complete.
   c. At summary checkpoints, prompt the LLM for a summary, reset the conversation, and continue.
3. Return a `LearningResult` with all memory IDs and total token usage.

### Manifest tracking

The `ContentManifest` (`crates/pupil-agent/src/learn/manifest.rs`) tracks the state of all learned sources. It is persisted as JSON at `/data/.pupil-manifest.json` inside the container.

Each source entry records:

- `content_hash`: SHA-256 hash of the source content.
- `prompt_hash`: SHA-256 hash of the learning prompt used (so prompt changes trigger re-learning).
- `memory_ids`: List of memory UUIDs created from this source.
- `last_learned`: ISO 8601 timestamp.
- `sync`: Optional sync state for URL sources (etag, last-modified, check/change counts, error tracking).

The manifest supports incremental learning:

- `is_unchanged()`: Returns `true` if both the content hash and prompt hash match the stored values. Unchanged sources are skipped.
- `find_orphaned_sources()`: Compares current source keys against stored keys. Sources present in the manifest but absent from the current curriculum are orphans whose memories should be deleted.
- Build records log each build's timestamp, model, token usage, cost estimate, and source/memory counts.

## 6. Agent Runtime

### ReAct loop

The `Agent` struct (`crates/pupil-agent/src/agent/mod.rs`) implements the ReAct (Reason + Act) pattern. Each user query triggers `handle_turn()`, which loops:

1. Send the full conversation history (including the system prompt, all prior messages, and tool results) to the LLM along with tool definitions from the MCP manager.
2. Record token usage.
3. Handle `MALFORMED_FUNCTION_CALL` with the retry mechanism (see section 3).
4. Push the assistant response to the conversation.
5. If tool calls are present (regardless of stop reason, since some providers like Vertex/Gemini return `EndTurn` even with tool calls): execute all tool calls in parallel via `execute_tools_parallel()`, push results to the conversation, and loop back to step 1.
6. If no tool calls: break on `EndTurn`, `MaxTokens`, `StopSequence`, `ContentFiltered`, or unknown stop reasons.
7. Check iteration limit (`max_iterations` from config) and token budget (`max_tokens_per_query`). Exceeding either raises an error.

### Conversation management

The `ConversationManager` (`crates/pupil-agent/src/conversation/mod.rs`) maintains the message history, session identity, and token counters for a single conversation.

- Messages are stored as a `Vec<Message>`. The first message is always the system prompt.
- `clear()` truncates to just the system prompt (retaining the first message).
- `compact_with_summary()` replaces the entire history with the system prompt plus a summary message, used during learning to prevent unbounded context growth.
- Tool results exceeding 8,192 bytes are truncated with a note indicating the original size.
- Token usage is tracked at both the session level (cumulative) and the turn level (reset on each `push_user()`).

### Session persistence

Sessions are saved as JSON files at `/data/sessions/{uuid}.json`. The `save()` method writes to a temp file first, then atomically renames it. `load()` reads and deserializes a session by UUID. `list_sessions()` scans the directory for `.json` files.

In interactive mode, the session is saved after the main loop exits (whether normally or on error). Session IDs can be passed via `--session` to resume a previous conversation.

### System prompt construction

The `SystemPromptBuilder` (`crates/pupil-agent/src/prompt/mod.rs`) assembles the system prompt from several sections, in order:

1. **Identity**: "You are {name}: {description}"
2. **Core Instructions**: Custom instructions from `system_prompt` in `pupil.yaml`.
3. **Memory Usage**: Automatically included if recalld tools (`recall_memories`, `store_memory`) are among the available tools. Instructs the agent to search for relevant memories before answering and to store new knowledge learned from users.
4. **Available Tools**: Lists each tool with its name, description, and parameter signatures.
5. **Other Available Agents**: If peer agents are configured (for multi-agent routing), lists them with descriptions so the agent can refer users to specialists.
6. **Response Guidelines**: Standard guidance on grounding answers in knowledge, expressing uncertainty, and synthesizing recalled memories naturally.

### Structured output (response schema)

When `response_schema` is configured in `pupil.yaml`, the schema flows through the system as follows:

1. **Config load**: Both `pupil-cli` (`ResponseSchemaConfig` in `agent_config.rs`) and `pupil-agent` (`ResponseSchemaConfig` in `config/mod.rs`) parse and validate the `response_schema` block. Validation checks the name format (`[a-zA-Z0-9_-]`, max 64 characters) and, when `strict` is `true`, requires the top-level schema `type` to be `"object"`.
2. **System prompt**: During server startup, the `SystemPromptBuilder` receives the schema via `with_response_schema()` and incorporates it into the system prompt so the model understands the expected output format.
3. **ChatConfig**: The schema is converted from `config::ResponseSchemaConfig` into the LLM layer's `ResponseSchema` struct and attached to the `ChatConfig` via `with_response_schema()`. This `ChatConfig` is passed to `LlmProvider::chat()` on every call.
4. **Per-request override**: In HTTP server mode, the `POST /v1/chat/completions` handler checks the request body for a `response_schema` field. If present, it takes precedence over the config-level schema for that request. If absent, the config-level schema is used. The effective schema (whichever source wins) is set on the `ChatConfig` before calling the LLM.
5. **Response validation**: After the ReAct loop completes, if a schema was active, the handler verifies that the final text is valid JSON. Truncated responses (finish reason `"length"`) produce a `structured_output_truncated` error. Other JSON parse failures produce a server error.

The `ResponseSchema` struct in the LLM layer (`crates/pupil-agent/src/llm/mod.rs`) carries four fields: `name`, `description` (optional), `schema` (a `serde_json::Value`), and `strict` (defaults to `true`). Each `LlmProvider` implementation is responsible for translating this into the provider's native structured output mechanism.

### Interactive vs server mode

**Interactive mode** (default, no subcommand or `--port`): Reads lines from stdin in a blocking task, runs `handle_turn()` for each non-empty line, and writes the assistant response to stdout. Handles SIGINT and SIGTERM for graceful shutdown. Sessions are persisted on exit.

**Server mode** (`--port N`): Starts an Axum HTTP server on the given port. The server exposes:

- `POST /v1/chat/completions`: OpenAI-compatible chat endpoint. Supports both synchronous responses and SSE streaming. Runs the full ReAct loop (including tool calls) internally before returning the final response. Accepts optional `session_id` for conversation continuity.
- `POST /v1/sessions`: Create a new session.
- `GET /v1/sessions/{id}`: Get session metadata (message count, token usage).
- `DELETE /v1/sessions/{id}`: Delete a session.
- `GET /health`: Returns server status, MCP server health, memory stats, and usage counters.
- `GET /metrics`: Placeholder for Prometheus metrics.

The server uses `DashMap` for concurrent session storage and `CancellationToken` for graceful shutdown. CORS is configured to allow all origins.

## 7. Self-Test Cycle

The self-test cycle validates that the agent has learned its curriculum well enough. It is configured via `build.self_test` in `pupil.yaml` and runs during `pupil build`. The `min_score` field defaults to 0.8, and `max_retries` defaults to 3.

### Test execution

Test cases are defined in a YAML file (e.g., `tests.yaml`). Each test case has a `question`, a `name`, a list of `expects` (assertions), and optionally a list of `sources` (curriculum files the question relates to).

The test runner (`run_test_mode` in `crates/pupil-agent/src/main.rs`) creates an `Agent` instance and, for each test case:

1. Resets the conversation to a clean state.
2. Sends the question via `run_single_query()`.
3. Evaluates all assertions against the response.
4. Supports retries per test case (configurable via `--retries`).

### Assertion types

The test system supports these assertion types (evaluated in `evaluate_assertion()`):

| Assertion | Behavior |
|---|---|
| `contains` | Case-insensitive substring check |
| `not_contains` | Fails if substring is present |
| `contains_any` | Passes if any candidate substring is found |
| `contains_all` | Passes only if all required substrings are found |
| `matches` | Regex match against the response |
| `not_matches` | Fails if regex matches |
| `starts_with` | Case-insensitive prefix check (trims leading whitespace) |
| `llm_judge` | Sends the question, response, and criteria to a judge LLM for scoring |

### LLM judge

The `llm_judge` assertion (`eval_llm_judge_in_container()`) calls a separate LLM (configurable via `--judge-model`, `config.judge_model` in the test file, or the agent's own model as fallback) with a scoring prompt:

- The judge scores the response from 0.0 to 1.0 on how well it meets the given criteria.
- The judge responds with JSON: `{"score": <float>, "reasoning": "<explanation>"}`.
- The response parser handles code fences, surrounding text, and JSON extraction.
- Scores are clamped to the [0.0, 1.0] range.
- The assertion passes if the score meets or exceeds the threshold (from the assertion config, test case, or global test config).
- Temperature is set to 0.0 for deterministic scoring.

### Source-linked remedial learning

During `pupil build`, if the self-test pass rate falls below `min_score`:

1. **Failure analysis** (`analyze_test_failures()` in `crates/pupil-cli/src/commands/build.rs`): Identifies which test cases failed, looks up their `sources` field from the test YAML, and collects the list of source files linked to failed tests.

2. **Remedial learning** (`run_remedial_learning()`): Re-runs the learning loop with:
   - A remedial prompt (base64-encoded to avoid shell escaping issues with Docker) that instructs the agent to re-read the specific source documents, look for gaps, missing details, and incorrect information, and use `find_similar_memories` to avoid storing duplicates.
   - The `--force-relearn` flag, which skips content hash checks so the agent re-processes even unchanged sources.
   - The `--source` flag repeated for each failed source, limiting re-learning to only the relevant documents.

3. **Retry loop**: Steps 1-2 repeat up to `max_retries` times. If the pass rate still does not reach the threshold, the build fails with a detailed error.

The remedial prompt never includes the test questions or expected answers. The agent only re-reads source material and decides what additional knowledge to store.

## 8. Build Pipeline

The `pupil build` command (`crates/pupil-cli/src/commands/build.rs`) orchestrates the full flow from curriculum scan to committed image. The pipeline has eight phases:

### Phase 1: Scan curriculum

- Resolve the agent directory and load `pupil.yaml`.
- Enumerate all curriculum sources (files, directories, URLs, globs).
- Hash each source with SHA-256.
- Load the manifest from the existing image (if any) by running a temporary container that reads `/data/.pupil-manifest.json`.
- Compare hashes to classify sources as: to-learn (new or changed), unchanged (skip), or removed (clean up).

### Phase 2: Start build infrastructure

- Pull the base image if not present locally.
- Detect whether Ollama is running on the host (by probing `http://localhost:11434/api/tags`).
- If Ollama is on the host: the build container accesses it via `host.docker.internal:11434`.
- If Ollama is not on the host: create a Docker network, start an Ollama sidecar container, wait for it to become ready (up to 120 seconds), and pull the `embeddinggemma` embedding model.
- Create and initialize a data volume with the correct directory structure and ownership (UID 65532 for the nonroot user).
- Build the environment variable map: LLM API keys, cloud credentials, recalld embedding configuration (`RECALLD_EMBEDDING_PROVIDER=ollama`, `RECALLD_EMBEDDING_MODEL=embeddinggemma:latest`, `RECALLD_EMBEDDING_DIMENSIONS=768`).

### Phase 3: Clean up removed sources

For each source present in the old manifest but absent from the current curriculum, run a `forget-source` container to delete associated memories from recalld.

### Phase 4: Learn curriculum

Run the agent container with the `learn` subcommand. The container's main process is `pupil-agent --config /tmp/pupil.yaml learn`, with curriculum bind-mounted at `/curriculum` and the data volume at `/data`.

Alternatively, the build container may be started in `idle` mode (`pupil-agent idle`) as a keep-alive process. In this configuration, the container stays running while the CLI orchestrates learning by sending `exec` commands into it, rather than running `learn` as the container's main process. This is particularly useful on distroless images that have no shell or `sleep` command to keep a container alive.

### Phase 5: Self-test cycle (optional)

If `build.self_test` is configured:

- Run the test suite inside a separate container (`pupil-agent test --json`).
- Analyze results and compute pass rate.
- If below threshold, run remedial learning and re-test, up to `max_retries` times.
- If the threshold is never met, the build fails.

See section 7 for details.

### Phase 6: Commit image

Call `docker commit` on the learning container to snapshot the `/data` volume (containing the populated recalld database and manifest) into a new OCI image.

### Phase 7: Cleanup

Remove the learning container, the Ollama sidecar (if used), the data volume, and the build network.

### Phase 8: Summary

Print the number of sources learned, sources skipped, sources removed, the final image name, and the build duration.

## 9. Multi-Agent Routing

The router (`crates/pupil-cli/src/router/mod.rs` and its submodules) allows multiple specialized Pupil agents to run behind a single HTTP endpoint. Incoming queries are routed to the most appropriate agent.

### Router architecture

The `RoutingEngine` is the central component. It holds:

- An `AgentRegistry` containing all configured agents with their URLs, descriptions, topics, exclusive topics, keywords, and health status.
- A `RoutingStrategyConfig` specifying which routing strategy to use.
- A `KeywordIndex` for keyword matching.
- An `EmbeddingIndex` and optional `EmbeddingClient` for semantic similarity.
- An optional `LlmClassifier` for LLM classification.
- Fallback behavior (route to a default agent, return an error, or ask the user to clarify).

### Routing strategies

Each strategy produces a `RoutingDecision` containing the chosen agent, a confidence score, the strategy tier that produced the decision, optional reasoning, and a list of alternatives.

**Exclusive topics** (always checked first): If a query contains a substring matching an agent's exclusive topic list, route to that agent with confidence 1.0. If multiple agents match, the one with the highest priority wins.

**Keyword matching** (`RoutingStrategyConfig::Keyword`): Matches query terms against each agent's keyword lists using the `KeywordIndex`.

**Embedding similarity** (`RoutingStrategyConfig::Embedding`): Computes the embedding of the query and finds the nearest agent description/topic vector in the `EmbeddingIndex` using the `EmbeddingClient`.

**LLM classification** (`RoutingStrategyConfig::Llm`): Sends the query and agent descriptions to an LLM classifier that returns the best-matching agent.

**Hybrid** (`RoutingStrategyConfig::Hybrid`): Runs strategies in a tiered cascade. Each tier has a configurable confidence threshold (`keyword_threshold`, `embedding_threshold`, `llm_threshold`). If a tier produces a result above its threshold, that result is returned without invoking subsequent tiers.

If the final confidence is below the global `confidence_threshold`, the fallback behavior is applied.

### Session affinity

The `SessionAffinityMap` ensures that follow-up messages within a conversation are routed to the same agent. It is a `DashMap` from session IDs to agent names, with a configurable TTL. Expired entries are evicted by a background task running every 60 seconds.

### Health checking

The `AgentRegistry` tracks the health of each agent. A background loop periodically calls each agent's health endpoint and updates its status. The router only considers healthy agents during routing decisions.

### HTTP endpoints

The router exposes:

- `POST /v1/chat/completions`: Routes the request to the best agent and proxies the response (including streaming). Checks session affinity before routing.
- `GET /v1/agents`: List all configured agents with their status.
- `GET /v1/agents/{name}`: Get details for a specific agent.
- `GET /health`: Router health status.
- `GET /metrics`: Prometheus metrics.

## 10. Inter-Agent Communication

When multiple agents run behind the router, they can call each other during conversations. An agent delegates a question to another agent by calling the `ask_agent` tool, which sends an HTTP request to the target agent's `/v1/chat/completions` endpoint (either directly or through the router) and returns the response content.

### Communication model

Inter-agent calls use the same OpenAI-compatible HTTP API that external clients use. The calling agent constructs a minimal chat completion request containing a single user message (the question) and sends it to the target. The target agent processes the request through its own full ReAct loop (including memory lookups and tool calls), then returns a chat completion response. The caller extracts the assistant message content and returns it as the `ask_agent` tool result.

The `ask_agent` tool is defined in `crates/pupil-agent/src/collaboration/tool.rs` and registered alongside MCP tools when collaboration is enabled. It takes two required parameters: `agent` (the target agent name) and `question` (a self-contained question string).

Agent discovery happens through the `PUPIL_AGENT_REGISTRY` environment variable, which contains a JSON array of `{name, url, description}` entries. The router sets this variable when starting agent containers. At startup, if collaboration is enabled in `pupil.yaml`, the agent runtime (`start_server()` in `crates/pupil-agent/src/server/mod.rs`) reads the registry, constructs an `AgentCaller`, and injects the `ask_agent` tool into the LLM's tool list.

The system prompt builder (`crates/pupil-agent/src/prompt/mod.rs`) adds an "Available Agents" section listing each peer agent's name and description, so the LLM knows which agents exist and what they specialize in.

### Routing inter-agent calls

When `PUPIL_ROUTER_URL` is set, inter-agent calls are sent to the router rather than directly to the target agent. The caller sets the `X-Pupil-Target-Agent` header to specify which agent should handle the request. The router checks this header and, if present, routes directly to the named agent (bypassing the normal routing strategy).

When `PUPIL_ROUTER_URL` is not set, the caller sends the request directly to the target agent's URL from the registry.

### Loop prevention

Three mechanisms prevent infinite call loops and runaway chains:

**Depth tracking (`X-Pupil-Depth` header):** Each inter-agent call increments a depth counter carried in the `X-Pupil-Depth` HTTP header. When an agent receives a request, it reads the current depth from this header (defaulting to 0 for external requests). Before calling another agent, it checks that `current_depth + 1` does not exceed `max_depth` from the collaboration config. The router also enforces a global depth limit: if an incoming request's `X-Pupil-Depth` exceeds `max_inter_agent_depth`, the router rejects it with a `400 Bad Request` before forwarding.

**Call chain tracking (`X-Pupil-Chain` header):** The `X-Pupil-Chain` header carries a comma-separated list of agent names that have participated in the current call chain. Before calling a target agent, the caller checks whether the target's name already appears in the chain. If it does, the call is rejected with a `LoopDetected` error. This prevents cycles like A -> B -> A.

When agent A calls agent B, the outgoing request includes:
- `X-Pupil-Depth: <current + 1>`
- `X-Pupil-Chain: <existing chain>,<self name>`
- `X-Pupil-Source: <self name>`

**Per-turn call limit:** The `max_calls_per_turn` config field (default 10) limits how many `ask_agent` tool calls can be made during a single user query. The counter is tracked in `execute_tools_parallel()` in `crates/pupil-agent/src/server/mod.rs`. Once the limit is reached, further `ask_agent` calls return an error tool result.

### Network topology

All agents and the router run as separate containers. In a typical Docker Compose deployment:

```
                    +-----------+
  External client --| Router    |
                    | :9090     |
                    +-----+-----+
                          |
               +----------+-----------+
               |                      |
        +------+------+      +-------+------+
        | Agent A     |      | Agent B      |
        | :8081       |      | :8082        |
        +-------------+      +--------------+
```

External requests enter through the router, which selects an agent. If agent A needs to call agent B, there are two paths depending on configuration:

1. **Through the router** (when `PUPIL_ROUTER_URL` is set): Agent A sends the request to the router with `X-Pupil-Target-Agent: agent-b`. The router forwards it to agent B. This keeps all traffic flowing through a single point, which simplifies network configuration and lets the router enforce its own depth limits.

2. **Direct** (when `PUPIL_ROUTER_URL` is not set): Agent A sends the request directly to agent B's URL from the registry. This reduces latency by one hop but requires agents to be able to reach each other's URLs directly.

The router forwards the `X-Pupil-Depth`, `X-Pupil-Chain`, and `X-Pupil-Source` headers to the target agent, preserving the call chain context across hops.

### Implementation files

| File | Role |
|---|---|
| `crates/pupil-agent/src/collaboration/mod.rs` | `AgentCaller` struct, registry parsing, call execution with depth/chain/timeout enforcement |
| `crates/pupil-agent/src/collaboration/tool.rs` | `ask_agent` tool definition and system prompt section builder |
| `crates/pupil-agent/src/server/mod.rs` | Server startup (wires collaboration into the tool list), request handler (reads depth/chain headers), `execute_tools_parallel()` (dispatches `ask_agent` calls with per-turn limits) |
| `crates/pupil-agent/src/prompt/mod.rs` | System prompt builder with peer agent injection |
| `crates/pupil-agent/src/config/mod.rs` | `CollaborationConfig` and `AllowedAgents` types with defaults |
| `crates/pupil-cli/src/agent_config.rs` | CLI-side `CollaborationConfig` (mirrors the agent-side config) |
| `crates/pupil-cli/src/router/proxy.rs` | Router proxy handler: forwards depth/chain/source headers, enforces router-level depth limit, supports `X-Pupil-Target-Agent` for directed routing |

## Embeddings

Embeddings are generated by Ollama using the `embeddinggemma` model (768 dimensions). During build, Ollama runs either:

- On the host (accessed by the container via `host.docker.internal:11434`)
- As a sidecar container on a shared Docker network

The embedding provider and model are configured via environment variables passed to recalld:

- `RECALLD_EMBEDDING_PROVIDER=ollama`
- `RECALLD_EMBEDDING_MODEL=embeddinggemma:latest`
- `RECALLD_EMBEDDING_BASE_URL=http://host.docker.internal:11434` (or the sidecar hostname)
- `RECALLD_EMBEDDING_DIMENSIONS=768`

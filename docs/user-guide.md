# User Guide

This guide walks through the full Pupil workflow, from creating your first agent to distributing it through a container registry.

## Prerequisites

Before using Pupil, you need:

1. **Rust 1.85+**: Install via [rustup](https://rustup.rs/).
2. **Docker or Podman**: Pupil uses containers to package agents. Install Docker from [docs.docker.com](https://docs.docker.com/get-docker/) or Podman from [podman.io](https://podman.io/getting-started/installation).
3. **Embeddings**: recalld (the memory system inside the container) needs an embedding provider to index and search memories. By default, Pupil uses Ollama with `embeddinggemma`. If Ollama is running on your host, the build connects to it automatically. If not, the build starts an Ollama sidecar container (no local install needed). You can also configure recalld to use a different embedding provider entirely.
4. **An LLM API key**: Set the appropriate environment variable for your provider:
   - Anthropic (Claude models): `ANTHROPIC_API_KEY`
   - OpenAI (GPT models): `OPENAI_API_KEY`
   - Google (Gemini models): `GOOGLE_API_KEY`
   - Vertex AI: `VERTEX_API_KEY` (plus `VERTEX_PROJECT_ID` and `VERTEX_LOCATION`), or `GOOGLE_APPLICATION_CREDENTIALS` for service account authentication
   - Azure OpenAI: `AZURE_OPENAI_API_KEY` (plus `AZURE_OPENAI_ENDPOINT`)
   - AWS Bedrock: `AWS_ACCESS_KEY_ID` (plus `AWS_SECRET_ACCESS_KEY`)
   - Ollama (local): no key needed

Run `pupil doctor` to validate that your environment is set up correctly.

## Installation

```bash
git clone https://github.com/calebevans/pupil.git
cd pupil
cargo install --path crates/pupil-cli

# Build the base container image (required before running pupil build)
docker build -t pupil-base:dev -f container/Dockerfile .
```

After installation, generate shell completions for tab completion:

```bash
# Bash
pupil completions bash > /etc/bash_completion.d/pupil

# Zsh
pupil completions zsh > ~/.zfunc/_pupil

# Fish
pupil completions fish > ~/.config/fish/completions/pupil.fish

# PowerShell
pupil completions powershell > pupil.ps1
```

If you want to skip ahead and try a working agent, see the [`examples/astronomy-agent`](../examples/astronomy-agent) directory for a complete example with curriculum, config, and tests.

## Creating an Agent

```bash
pupil create hr-assistant
```

This creates a directory called `hr-assistant/` containing:

- `pupil.yaml`: the agent configuration file
- `curriculum/`: an empty directory where you place curriculum files

### Templates

Use `--template` to start from a preconfigured setup:

```bash
pupil create hr-assistant --template full
```

| Template | Description |
|---|---|
| `minimal` (default) | Bare-bones config with recalld and a curriculum directory. Good starting point. |
| `full` | All configuration sections included with commented-out examples for self-test, URL sources, MCP servers, runtime limits, and budget controls. |
| `knowledge-base` | Tuned for Q&A over documents. System prompt instructs the agent to always search memories before answering and to ground answers in retrieved knowledge. |
| `chatbot` | Conversational assistant that stores new information learned from users. |

### Create options

| Flag | Description |
|---|---|
| `-t` / `--template <name>` | Template to use: `minimal`, `full`, `knowledge-base`, `chatbot` |
| `-m` / `--model <model>` | LLM model identifier (default: `claude-sonnet-4-6`, or your global default) |

### Agent name rules

Agent names must be 1-64 characters, contain only lowercase letters, digits, and hyphens, and start and end with a letter or digit. The names `base`, `runtime`, `build`, and `dev` are reserved.

### The pupil.yaml file

Here is what a realistic `pupil.yaml` looks like for an engineering onboarding assistant:

```yaml
name: hr-assistant
description: "Onboarding assistant for the engineering team"
model: claude-haiku-4
learning_model: claude-sonnet-4-6
system_prompt: |
  You are an onboarding assistant for the engineering team.
  Use your memory tools to find relevant information before answering.
  If you don't know something, say so.

mcp_servers:
  recalld:
    command: recalld
    args: ["mcp"]
    required: true
    env:
      RECALLD_DATA_DIR: /data/recalld

curriculum:
  namespace: knowledge
  decay: 0.0
  sources:
    - ./curriculum/
    - url: https://wiki.internal/eng-handbook

build:
  max_cost_usd: 10.00
  on_budget_exceeded: confirm
  self_test:
    file: tests.yaml
    min_score: 0.8
    max_retries: 3
```

Key fields:

- `model`: the LLM used at runtime (when users chat with the agent).
- `learning_model`: (optional) a separate, potentially more capable model used during `pupil build` to comprehend curriculum and store memories. If omitted, `model` is used for both learning and runtime.
- `system_prompt`: the agent's persona and instructions.
- `mcp_servers`: MCP tool servers the agent can use. The `recalld` server is always required (it provides memory tools).
- `curriculum.sources`: a list of local paths and/or URLs that comprise the agent's curriculum.
- `build.self_test`: (optional) run a test suite after learning and re-study sources linked to failed questions.

## Writing Curriculum

The agent's curriculum is the body of knowledge it will learn during `pupil build`. You provide curriculum as files in the `curriculum/` directory, or as URLs in `pupil.yaml`.

### Supported file formats

`.md`, `.txt`, `.pdf`, `.html`, `.htm`, `.json`, `.csv`, `.yaml`, `.yml`

### How learning works

During `pupil build`, an LLM reads each curriculum source, comprehends the content, and decides what to store as memories using the recalld MCP tools. This means:

- The LLM extracts concepts, procedures, facts, and relationships from the text.
- It decides how to tag, categorize, and link memories.
- It checks for duplicate information before storing.
- It creates a structured knowledge graph, not a flat list of text chunks.

The agent understands the material and stores what it considers important, much like a person studying a document.

### Directory structure

You can organize curriculum files however you want inside `curriculum/`. Subdirectories are traversed recursively during builds:

```
hr-assistant/
  pupil.yaml
  curriculum/
    company-handbook.md
    dev-setup-guide.md
    api-reference/
      authentication.md
      endpoints.md
    runbooks/
      deploy-to-staging.md
      incident-response.md
```

### URL sources

URLs are declared in `pupil.yaml` under `curriculum.sources` and fetched during `pupil build`:

```yaml
curriculum:
  sources:
    - ./curriculum/
    - url: https://wiki.internal/eng-handbook
    - url: https://docs.example.com/api-reference
      sync:
        interval: 1h
```

URL sources can be configured with sync intervals, authentication, and change detection strategies. See [Watching and Syncing](#watching-and-syncing) for details.

### Per-source learning profiles

You can assign different learning profiles to different sources:

```yaml
curriculum:
  sources:
    - path: ./curriculum/api-reference/
      learning_profile: reference
    - path: ./curriculum/runbooks/
      learning_profile: procedural
      tags:
        - team/sre
```

## Teaching

The `pupil teach` command copies files into an agent's `curriculum/` directory and registers URL sources in `pupil.yaml`.

The agent name argument is optional if you are inside the agent directory (where `pupil.yaml` exists).

### Adding local files

```bash
# Add individual files
pupil teach hr-assistant /path/to/handbook.md /path/to/api-docs.txt

# Add a directory (non-recursive by default)
pupil teach hr-assistant /path/to/docs/

# Add a directory recursively
pupil teach hr-assistant -r /path/to/docs/

# Filter with a glob pattern
pupil teach hr-assistant -r --glob "*.md" /path/to/docs/

# Dry run (preview what would be added)
pupil teach hr-assistant --dry-run /path/to/docs/
```

Files are copied into the agent's `curriculum/` directory. When adding a directory, the directory structure is preserved inside `curriculum/`.

### Adding URL sources

```bash
pupil teach hr-assistant --url https://docs.example.com/guide
```

This adds the URL to the `curriculum.sources` list in `pupil.yaml`. The content is fetched and learned during `pupil build`.

### Deduplication

Duplicate content is detected via SHA-256 hashing and skipped. If you add a file whose content already exists in the curriculum (even under a different name), Pupil skips it and reports how many duplicates were found.

### Teach options

| Flag | Description |
|---|---|
| `-r` / `--recursive` | Traverse directories recursively |
| `-g` / `--glob <pattern>` | Filter files by glob pattern (e.g., `"*.md"`) |
| `--url <url>` | Add a URL source to `pupil.yaml` instead of copying a file |
| `--dry-run` | Preview what would be added without modifying anything |

## Building

```bash
cd hr-assistant
pupil build
```

You can also pass the agent name explicitly from any directory:

```bash
pupil build hr-assistant
```

The build process runs through these phases:

1. **Scan curriculum**: finds all source files and URLs, computes content hashes, and compares against the previous build manifest to determine what changed.
2. **Start build infrastructure**: connects to your host Ollama (if running) or starts an Ollama sidecar container for embeddings.
3. **Clean up removed sources**: if any sources were deleted since the last build, their memories are removed.
4. **Learn curriculum**: runs the agent in learning mode. The LLM reads each changed source and stores memories via recalld MCP tools.
5. **Self-test cycle** (if configured): runs the test suite. If the pass rate is below the threshold, the agent re-studies the specific sources linked to failed questions (without seeing the test answers). This repeats up to `max_retries` times.
6. **Commit image**: snapshots the container (including all learned memories in `/data/`) into an OCI image.
7. **Cleanup**: removes the build container, Ollama sidecar, and temporary volumes.

### Incremental builds

By default, `pupil build` only re-learns sources whose content has changed since the last build. The build manifest (stored at `/data/.pupil-manifest.json` inside the image) tracks content hashes for each source. Unchanged sources are skipped entirely. Use `--no-cache` to force a full re-learn of all sources.

### Cost estimation

Use `--dry-run` to preview estimated token usage and cost before building:

```bash
pupil build --dry-run
```

This shows the number of sources to learn, estimated input tokens, estimated cost range, and any configured budget limits. No learning is performed.

### Self-test cycle

When `build.self_test` is configured in `pupil.yaml`, the build includes a test-learn-retest loop:

```yaml
build:
  self_test:
    file: tests.yaml
    min_score: 0.8
    max_retries: 3
    judge_model: claude-sonnet-4-6  # optional: use a different model to judge answers
```

Each test case in `tests.yaml` can specify which source files are relevant via the `sources` field. When a test fails, only those specific sources are re-studied. The agent never sees the test answers during remedial learning.

### Build options

| Flag | Description |
|---|---|
| `--no-cache` | Re-learn all sources, ignoring content hashes from the previous build |
| `--no-confirm` | Skip confirmation prompts (e.g., when the budget is exceeded) |
| `--dry-run` | Show what would be learned without actually running |
| `--runtime <name>` | Override the container runtime (`docker` or `podman`) |
| `--tag <tag>` | Image tag (default: `latest`) |
| `--progress <mode>` | Progress display mode (default: `auto`). Not yet functional in the current implementation. |

## Running

After building, run your agent:

### Interactive mode

```bash
pupil run
```

You can also pass the agent name explicitly:

```bash
pupil run hr-assistant
```

This starts the agent container and attaches your terminal for a conversation. Type messages and receive responses. Press Ctrl+C to stop. In interactive mode, stopping the agent also removes the container.

If the agent is already running, `pupil run` attaches to the existing container instead of starting a new one.

In detached mode (`--detach`), the container persists until you explicitly stop it.

### HTTP server mode

```bash
pupil run --port 8080
```

Starts the agent as an HTTP server accessible at `http://localhost:8080`.

### Background mode

```bash
pupil run --port 8080 --detach
```

Runs the agent in the background. Use `pupil logs` to view output and `pupil status` to check on it.

### With Ollama sidecar

```bash
pupil run --with-ollama
```

Starts an Ollama container alongside the agent via Docker Compose. This is useful if your host does not have Ollama installed. The agent connects to the sidecar for embeddings.

### Passing environment variables

```bash
pupil run -e WEB_SEARCH_KEY=sk-abc123 -e CUSTOM_VAR=value
```

API keys for the configured model are passed through automatically. Use `-e` for additional variables that MCP servers or other components need.

### Run options

| Flag | Description |
|---|---|
| `--port <port>` | Run as an HTTP server on this port |
| `-d` / `--detach` | Run in the background |
| `-e KEY=VALUE` / `--env KEY=VALUE` | Pass additional environment variables |
| `--tag <tag>` | Use a specific image tag (default: `latest`) |
| `--with-ollama` | Start an Ollama sidecar alongside the agent (via Docker Compose) |

## Structured Output

By default, agents respond with free-form text. When you need the agent to return JSON in a specific format (for example, to feed results into another system), configure a `response_schema` in `pupil.yaml`.

### When to use structured output

Structured output is useful when:

- The agent's response will be parsed by code rather than read by a person.
- You need predictable fields, types, and enumerations in every response.
- Downstream systems require a specific JSON contract (classification labels, extracted entities, scored evaluations).

If the agent is primarily conversational, structured output is unnecessary.

### Configuring in pupil.yaml

Add a `response_schema` block at the top level of your agent config:

```yaml
name: ticket-classifier
model: claude-haiku-4
system_prompt: |
  You are a support ticket classifier.
  Analyze the user's message and classify it.

response_schema:
  name: ticket_classification
  description: "Classify a support ticket"
  strict: true
  schema:
    type: object
    properties:
      category:
        type: string
        enum: [billing, technical, account, other]
      priority:
        type: string
        enum: [low, medium, high, critical]
      summary:
        type: string
      confidence:
        type: number
    required: [category, priority, summary, confidence]
```

With this configuration, every response from the agent will be a JSON object matching the schema. The `strict: true` setting (the default) tells the LLM provider to use constrained decoding, which guarantees the output conforms to the schema.

See the [Configuration Reference](configuration.md#response_schema) for the full field list.

### Per-request schema overrides via the HTTP API

When running in HTTP server mode (`pupil run --port 8080`), callers can override the schema on a per-request basis by including a `response_schema` field in the request body. This takes precedence over the schema in `pupil.yaml` for that request only.

```bash
curl -X POST http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [
      {"role": "user", "content": "Extract the meeting details from this text: Team sync at 3pm on Friday in Room 204 with Alice and Bob."}
    ],
    "response_schema": {
      "name": "meeting_details",
      "description": "Extracted meeting information",
      "strict": true,
      "schema": {
        "type": "object",
        "properties": {
          "title": {"type": "string"},
          "time": {"type": "string"},
          "location": {"type": "string"},
          "attendees": {
            "type": "array",
            "items": {"type": "string"}
          }
        },
        "required": ["title", "time", "location", "attendees"]
      }
    }
  }'
```

If neither the request nor `pupil.yaml` specifies a schema, the agent responds with free-form text as usual.

### Schema validation

Pupil validates the schema at two points:

1. **Config load time**: When `pupil.yaml` is parsed, the `response_schema` name is checked against the pattern `[a-zA-Z0-9_-]` (max 64 characters). If `strict` is `true`, the top-level schema `type` must be `"object"`.
2. **Request time**: The HTTP API applies the same name validation for per-request schemas. After the LLM responds, Pupil verifies the output is valid JSON. If the response was truncated (due to a max output tokens limit), the API returns a `structured_output_truncated` error. If the output is not valid JSON for any other reason, it returns a server error.

### Interaction with tools

Structured output applies to the agent's final text response. Tool calls during the ReAct loop (memory lookups, MCP tool invocations) are unaffected. The schema constraint is applied to the `ChatConfig` that governs the LLM call, so the model knows throughout the conversation that its final answer must conform to the schema.

## Distributing

### Push to a registry

```bash
pupil push registry.example.com/hr-assistant:v1
```

An optional agent name can precede the registry ref:

```bash
pupil push hr-assistant registry.example.com/hr-assistant:v1
```

Before pushing, Pupil checks the image for environment variables that look like secrets (API keys, tokens, passwords). If any are found, the push is blocked unless you pass `--force`. API keys should be injected at runtime via `-e`, not baked into images.

| Flag | Description |
|---|---|
| `--latest` | Also tag and push as `latest` |
| `--force` | Push even if the image may contain secrets |

### Pull from a registry

```bash
pupil pull registry.example.com/hr-assistant:v1
```

The pulled image is re-tagged locally so you can run it with `pupil run`. The local name is inferred from the image name (stripping the `pupil-agent-` prefix if present), or you can override it:

```bash
pupil pull registry.example.com/hr-assistant:v1 --name my-assistant
```

| Flag | Description |
|---|---|
| `--name <name>` | Override the local agent name |

### Export to a tar file

For offline distribution without a registry:

```bash
pupil export --output hr-assistant.tar
```

If `--output` is not specified, the file is named `<agent-name>.tar`.

| Flag | Description |
|---|---|
| `-o` / `--output <path>` | Output file path |
| `--tag <tag>` | Image tag to export (default: `latest`) |

### Import from a tar file

```bash
pupil import hr-assistant.tar
```

The imported image is registered locally. You can override the name:

```bash
pupil import hr-assistant.tar --name my-assistant
```

| Flag | Description |
|---|---|
| `--name <name>` | Override the local agent name |

## Inspecting Knowledge

The `pupil inspect` command lets you examine what your agent has learned. All subcommands launch a temporary read-only container from the agent's image to query recalld.

If no subcommand is given, `pupil inspect` defaults to `list`.

### Global inspect options

These flags work with all inspect subcommands:

| Flag | Description |
|---|---|
| `--json` | Output as JSON |
| `--source <filter>` | Filter memories by source file name (substring match) |
| `--namespace <ns>` | Override the memory namespace |

### List all memories

```bash
pupil inspect list
pupil inspect list --sort strength
pupil inspect list --tag type/procedure
pupil inspect list --limit 20
```

Sort options: `source` (default), `strength`, `created`, `id`.

The output table shows each memory's short ID, source file, summary, strength (0.0-1.0), and phase (`full`, `summary`, or `ghost`).

### Show a specific memory

```bash
pupil inspect show <memory-id>
```

Displays the full details of a memory: summary, full text, source, namespace, strength, phase, timestamps, entities, topics, tags, and all relationships to other memories.

You can use short ID prefixes (at least 4 hex characters):

```bash
pupil inspect show 3a7f
```

If the prefix is ambiguous (matches multiple memories), Pupil shows the matches so you can be more specific.

### Search memories

```bash
pupil inspect search "deployment process"
pupil inspect search "API authentication" --limit 5 --min-score 0.5
```

Performs a semantic search across the agent's memories. Results are ranked by a combination of similarity and memory strength.

| Flag | Description |
|---|---|
| `--limit <n>` | Maximum results (default: 10) |
| `--min-score <f>` | Minimum similarity score threshold (0.0-1.0) |
| `--min-strength <f>` | Minimum memory strength threshold (0.0-1.0) |

If semantic search is unavailable (e.g., the embedding model is not loaded), Pupil falls back to full-text search and displays a warning.

### View statistics

```bash
pupil inspect stats
```

Shows:
- Total memory count
- Phase breakdown (full/summary/ghost) with percentages
- Memory count per source file, with bar chart visualization
- Type tag distribution (e.g., `type/procedure`, `type/reference`)
- Top entities by mention count

You can filter stats to a specific source:

```bash
pupil inspect stats --source handbook
```

### Quality report

```bash
pupil inspect quality
```

Scans the knowledge base for issues:

- **Near-duplicates**: memory pairs with similarity above 0.90 (configurable via `--duplicate-threshold`)
- **Orphaned memories**: memories with a source tag that are not tracked in the build manifest
- **Missing metadata**: memories without entities, topics, or source tags
- **Weak memories**: memories with strength below 0.50 (configurable via `--weak-threshold`)
- **Superseded chains**: old memories that were superseded but not removed
- **Empty sources**: source files that produced zero memories

Use `--fix` for interactive cleanup, which prompts you to merge duplicates, remove orphans, and clean up superseded chains:

```bash
pupil inspect quality --fix
```

| Flag | Description |
|---|---|
| `--duplicate-threshold <f>` | Similarity threshold for near-duplicate detection (default: 0.90) |
| `--weak-threshold <f>` | Strength threshold for weak memory detection (default: 0.50) |
| `--fix` | Interactive cleanup mode |

### Knowledge graph

```bash
pupil inspect graph
```

Shows a summary of the memory knowledge graph: total memories, total relationships, number of connected components, isolated memories, and edge type distribution.

To view the relationships for a specific memory:

```bash
pupil inspect graph --memory 3a7f --depth 2
```

To export the full graph as Graphviz DOT format:

```bash
pupil inspect graph --dot > knowledge.dot
dot -Tpng knowledge.dot -o knowledge.png
```

| Flag | Description |
|---|---|
| `--memory <id>` | Show relationships for a specific memory |
| `--depth <n>` | BFS depth for relationship traversal (default: 1, max: 3) |
| `--dot` | Output as Graphviz DOT format |

### Diff between image tags

```bash
pupil inspect diff v1 v2
```

Compares two tagged versions of the same agent. Shows memories added, removed, and unchanged, grouped by source. Useful for understanding what changed between builds.

You can filter the diff to a specific source:

```bash
pupil inspect diff v1 v2 --source api-reference
```

## Testing

The `pupil test` command runs a test suite against a built agent image to verify it learned the curriculum correctly.

### Writing tests

Create a `tests.yaml` file in your agent directory:

```yaml
config:
  temperature: 0
  retries: 1
  timeout_secs: 30
  threshold: 0.8

tests:
  - name: knows-deploy-process
    question: "How do I deploy to staging?"
    sources:
      - curriculum/runbooks/deploy-to-staging.md
    expects:
      - memory_hit: true
      - contains: "staging"
      - contains_any: ["kubectl", "helm", "docker"]

  - name: knows-tech-stack
    question: "What database does the payments service use?"
    expects:
      - contains: "PostgreSQL"
      - memory_source: api-reference/architecture.md

  - name: explains-review-process
    question: "What is our code review process?"
    expects:
      - llm_judge:
          criteria: "The response should describe the steps for submitting and reviewing a pull request, including who approves and any required checks."
          threshold: 0.7

  - name: admits-ignorance
    question: "What is the company's pet policy?"
    expects:
      - memory_hit: false
      - llm_judge:
          criteria: "The agent should honestly state that it does not have information about the pet policy."
```

### Test configuration

The `config` section sets defaults for all tests:

| Field | Description |
|---|---|
| `temperature` | LLM temperature for test queries (default: 0) |
| `retries` | Number of retries for flaky tests (default: 0) |
| `timeout_secs` | Timeout per test case in seconds (default: 30) |
| `threshold` | Default pass threshold for scored assertions like `llm_judge` (default: 0.8) |
| `judge_model` | (optional) Model to use for LLM-judge evaluations |

### Assertion types

**Text assertions** (evaluated locally, no LLM call):

| Assertion | Description |
|---|---|
| `contains: "text"` | Response contains the string (case-insensitive) |
| `not_contains: "text"` | Response does not contain the string |
| `contains_any: [a, b, c]` | Response contains at least one of the strings |
| `contains_all: [a, b, c]` | Response contains all of the strings |
| `matches: "regex"` | Response matches the regular expression |
| `not_matches: "regex"` | Response does not match the regular expression |
| `starts_with: "text"` | Response starts with the string (case-insensitive, trims whitespace) |

**Retrieval assertions** (check memory tool behavior):

| Assertion | Description |
|---|---|
| `memory_hit: true/false` | Agent called `recall_memories` and got results (or did not) |
| `memory_source: "file.md"` | A recalled memory came from the specified source file |
| `memory_query: "substring"` | The query passed to `recall_memories` contains the substring |
| `tool_called: "tool_name"` | The specified tool was called during the response |
| `tool_not_called: "tool_name"` | The specified tool was not called |

**Operational assertions**:

| Assertion | Description |
|---|---|
| `latency_ms: {max: 5000}` | Response latency is at most N milliseconds |
| `token_count: {max: 3000}` | Total token usage (input + output) is at most N |

**Scored assertions** (require an LLM call or embeddings):

| Assertion | Description |
|---|---|
| `llm_judge: {criteria: "...", threshold: 0.8}` | An LLM scores the response against the criteria (0.0-1.0) |
| `semantic_similarity: {reference: "...", threshold: 0.8}` | Cosine similarity between response and reference text embeddings |
| `faithfulness: {threshold: 0.8}` | Checks that claims in the response are supported by recalled context |

Each scored assertion can override the default `threshold` set in `config`.

### Test case fields

| Field | Description |
|---|---|
| `name` | Unique test name (kebab-case, alphanumeric with hyphens/underscores) |
| `question` | The question to ask the agent |
| `description` | (optional) Human-readable description |
| `context` | (optional) Context string for `faithfulness` assertions (if not provided, recalled memories are used) |
| `tags` | (optional) Tags for filtering tests (e.g., `["critical", "deploy"]`) |
| `sources` | (optional) Source files relevant to this question. Used by the self-test cycle to target remedial learning. |
| `threshold` | (optional) Per-test override for scored assertion thresholds |
| `expects` | List of assertions (at least one required) |

### Running tests

```bash
# Run all tests (default file: tests.yaml)
pupil test

# Run tests from a specific file
pupil test --file integration-tests.yaml

# Filter tests by name or tag (glob patterns supported)
pupil test --filter "deploy*"
pupil test --filter "*critical*"

# Override retries, timeout, or threshold
pupil test --retries 2 --timeout 60 --threshold 0.9

# Output as JSON
pupil test --json

# Export JUnit XML (for CI systems)
pupil test --junit results.xml

# Show detailed assertion results
pupil test --show-details
```

### Generating tests

Pupil can generate test cases from your curriculum using the configured LLM:

```bash
pupil test --generate --count 20 --output tests.yaml
```

The generated tests include a mix of factual recall, conceptual understanding, and negative tests (questions the agent should not be able to answer). Review and edit the generated tests before using them.

| Flag | Description |
|---|---|
| `--file <path>` | Test file path (default: `tests.yaml`) |
| `--filter <pattern>` | Filter tests by name or tag (glob patterns) |
| `--retries <n>` | Override retry count |
| `--timeout <secs>` | Override timeout per test case |
| `--threshold <f>` | Override pass threshold for scored assertions |
| `--json` | Output results as JSON |
| `--junit <path>` | Export results as JUnit XML |
| `--show-details` | Show detailed assertion results. Not yet functional; assertion details for failed tests are always shown. |
| `--generate` | Generate test cases from curriculum |
| `--count <n>` | Number of tests to generate (default: 10) |
| `--output <path>` | Write generated tests to a file (otherwise printed to stdout) |

## Watching and Syncing

### Watch: live development mode

During development, `pupil watch` monitors your curriculum directory and `pupil.yaml` for changes. When a file changes, it re-learns that file immediately and lets you chat with the agent in the same terminal session.

```bash
pupil watch hr-assistant
```

Watch mode:
- Starts a development container with the curriculum directory bind-mounted read-only. Changes to files on the host are detected by the filesystem watcher.
- Runs an initial learning pass (unless `--no-initial-learn` is set).
- Watches for file system changes (create, modify, delete) in `curriculum/` and `pupil.yaml`.
- When a curriculum file changes, re-learns only that file (forgets old memories from it first, then learns the new content).
- When a curriculum file is deleted, forgets the memories from that file.
- When `pupil.yaml` changes, detects whether learning profiles or runtime config changed and acts accordingly.
- Provides a live chat interface while watching.

```bash
# Watch and run tests after each re-learn
pupil watch hr-assistant --test

# Watch with a custom test file
pupil watch hr-assistant --test --file integration-tests.yaml

# Set debounce interval (default: 500ms)
pupil watch hr-assistant --debounce 1000

# Skip the initial full learning pass
pupil watch hr-assistant --no-initial-learn
```

| Flag | Description |
|---|---|
| `--test` | Run the test suite after each successful re-learn |
| `--file <path>` | Path to the test file (only used with `--test`) |
| `--debounce <ms>` | Debounce timeout in milliseconds (default: 500, range: 100-10000) |
| `--no-initial-learn` | Skip the initial full learning pass on startup |

### Sync: URL source updates

If your curriculum includes URL sources, `pupil sync` checks them for changes using HTTP conditional requests (ETags, Last-Modified headers) and content hashing.

```bash
# Check all URL sources for changes
pupil sync

# Force re-fetch all URLs regardless of changes
pupil sync --force

# Check a single URL
pupil sync --source https://wiki.internal/eng-handbook

# Preview changes without re-learning
pupil sync --dry-run

# Output results as JSON
pupil sync --json

# Run continuously on the configured interval
pupil sync --daemon
```

Sync configuration in `pupil.yaml`:

```yaml
curriculum:
  sync:
    enabled: true
    interval: 6h          # Global check interval (minimum: 5m)
    on_change: in_place   # in_place, rebuild, or notify
    concurrency: 4
    request_delay_ms: 500
    timeout_secs: 30
    respect_robots_txt: true
  sources:
    - url: https://wiki.internal/eng-handbook
    - url: https://wiki.internal/on-call-rotation
      sync:
        interval: 1h     # Per-source interval override
    - url: https://confluence.internal/display/ENG/deploy-guide
      sync:
        strategy: auto   # auto, confluence_api, notion_api, sitemap, webhook
        auth:
          type: bearer
          token: ${CONFLUENCE_API_TOKEN}
```

Authentication types: `bearer` (token), `basic` (username + password), `header` (custom headers). Environment variable references like `${VAR_NAME}` are resolved at runtime.

In daemon mode (`--daemon`), Pupil runs continuously and checks sources at their configured intervals. Consecutive errors trigger exponential backoff (up to 24 hours).

| Flag | Description |
|---|---|
| `--force` | Re-fetch all URLs regardless of changes |
| `--source <url>` | Sync a single URL source |
| `--dry-run` | Check for changes without re-learning |
| `--json` | Output results as JSON |
| `--daemon` | Run continuously on the configured interval |

## Multi-Agent Routing

The `pupil router` command manages a multi-agent router that routes queries to specialized agents. Each agent declares its routing configuration in `pupil.yaml`:

```yaml
routing:
  topics: [onboarding, dev-setup, tooling]
  exclusive_topics: []
  sample_questions:
    - "How do I set up my development environment?"
    - "What is the code review process?"
  priority: 0
```

The router subcommands are:

| Subcommand | Description |
|---|---|
| `pupil router start` | Start the multi-agent router |
| `pupil router stop` | Stop the router |
| `pupil router status` | Show router status |
| `pupil router test <query>` | Test which agent would handle a query |
| `pupil router add <name> <url>` | Add an agent endpoint to the router |
| `pupil router remove <name>` | Remove an agent from the router |
| `pupil router generate-config` | Generate router configuration from agent configs |

Note: the router is currently in development and not yet fully implemented.

## Inter-Agent Communication

When multiple agents run behind a router, they can call each other during conversations. This lets a generalist agent delegate domain-specific questions to a specialist, or lets two specialists collaborate to answer a question that spans both of their areas.

Inter-agent communication uses the `collaboration` section in `pupil.yaml`. Each participating agent must have `collaboration.enabled` set to `true`.

### How it works

When collaboration is enabled, the agent receives an `ask_agent` tool that it can call during the normal ReAct loop. The tool takes two parameters: `agent` (the name of the target agent) and `question` (a self-contained question to send). The calling agent includes all necessary context in the question, since the target agent has no access to the caller's conversation history.

The agent's system prompt is automatically augmented with an "Available Agents" section listing all peer agents and their descriptions. The LLM uses this information to decide when delegation is appropriate.

### Setting up two collaborating agents

Here is a complete example with two agents: an onboarding assistant and a database expert. Both run behind the router and can call each other.

**onboarding-bot/pupil.yaml:**

```yaml
name: onboarding-bot
description: "Onboarding assistant for the engineering team"
model: claude-haiku-4
system_prompt: |
  You are an onboarding assistant for the engineering team.
  Answer questions about dev setup, tooling, and team processes.

mcp_servers:
  recalld:
    command: recalld
    args: ["mcp"]
    required: true
    env:
      RECALLD_DATA_DIR: /data/recalld

curriculum:
  sources:
    - ./curriculum/

routing:
  topics: [onboarding, dev-setup, tooling, ci-cd]
  sample_questions:
    - "How do I set up my development environment?"

collaboration:
  enabled: true
  allowed_agents:
    - db-expert
  max_depth: 2
  timeout_secs: 60
```

**db-expert/pupil.yaml:**

```yaml
name: db-expert
description: "Database architecture and query optimization expert"
model: claude-haiku-4
system_prompt: |
  You are a database expert. Answer questions about schema design,
  query optimization, migrations, and database operations.

mcp_servers:
  recalld:
    command: recalld
    args: ["mcp"]
    required: true
    env:
      RECALLD_DATA_DIR: /data/recalld

curriculum:
  sources:
    - ./curriculum/

routing:
  topics: [database, sql, migrations, schema]
  exclusive_topics: [schema-design]
  sample_questions:
    - "How do I add an index to the users table?"

collaboration:
  enabled: true
  allowed_agents:
    - onboarding-bot
  max_depth: 2
  timeout_secs: 60
```

Build both agents, then start them behind the router:

```bash
pupil build onboarding-bot
pupil build db-expert

pupil run onboarding-bot --port 8081 --detach
pupil run db-expert --port 8082 --detach

pupil router start
pupil router add onboarding-bot http://localhost:8081
pupil router add db-expert http://localhost:8082
```

When a user asks the onboarding bot "What database does our payments service use and how should I connect to it?", the onboarding bot can use `ask_agent` to delegate the database-specific part to `db-expert`, then combine both answers into a single response.

### Controlling which agents can be called

The `allowed_agents` field restricts which agents can be called. Set it to `"all"` to allow calling any agent in the registry, or provide a list of specific agent names:

```yaml
# Allow all agents
collaboration:
  enabled: true
  allowed_agents: "all"

# Allow only specific agents
collaboration:
  enabled: true
  allowed_agents:
    - db-expert
    - payments-bot
```

### Safety limits

Three mechanisms prevent runaway inter-agent calls:

1. **Depth limit** (`max_depth`, default 3): Tracks how deep the call chain is. Agent A calling agent B is depth 1. Agent B calling agent C is depth 2. When the depth exceeds `max_depth`, the call is rejected.

2. **Loop detection**: The `X-Pupil-Chain` header carries the list of agents in the current call chain. If an agent appears in the chain it is about to be called into, the call is rejected to prevent cycles (e.g., A calls B calls A).

3. **Per-turn call limit** (`max_calls_per_turn`, default 10): Caps the number of `ask_agent` calls a single user query can trigger. This prevents an agent from making excessive delegations in one turn.

For more details on the protocol and headers, see the [Architecture](architecture.md#10-inter-agent-communication) documentation.

## Committing Runtime State

If an agent learns new information at runtime (e.g., through user conversations that trigger `store_memory` calls), you can snapshot its current state into a new image:

```bash
pupil commit hr-assistant
```

The agent must be running. This creates a new image that includes everything learned since the last build.

| Flag | Description |
|---|---|
| `--tag <tag>` | Image tag for the committed image (default: `latest`) |
| `-m` / `--message <msg>` | Commit message |

## Listing Agents

```bash
pupil list
pupil list --json
```

Shows all locally registered agent images with their name, tag, size, and build date.

| Flag | Description |
|---|---|
| `--json` | Output as JSON |

## Shell Completions

The `pupil completions` command generates shell completion scripts for tab completion.

```bash
pupil completions <shell>
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`.

See the [Installation](#installation) section for setup examples.

## Troubleshooting

### Doctor

Run `pupil doctor` to validate your environment:

```bash
pupil doctor
pupil doctor --details
```

It checks:
- **Container runtime**: Docker or Podman is installed and working
- **Global config**: `~/.config/pupil/config.yaml` exists and is valid
- **API keys**: at least one of `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `GOOGLE_API_KEY` is set. Other supported provider keys (Vertex AI, Azure OpenAI, AWS Bedrock) are not checked by `doctor`.
- **Ollama**: reachable at `http://localhost:11434` (or `OLLAMA_BASE_URL`) and has the `embeddinggemma` model
- **Base image**: `pupil-base:dev` is available locally. If not found, `pupil build` attempts to pull it automatically. For local development, build it manually as described in the Installation section.

Use `--details` to show extra information like the list of Ollama models, config file values, and the expected config path.

### Status

```bash
pupil status hr-assistant
pupil status --json
```

Shows the agent's model, running/stopped state, image name, image size, memory count, and last build details (date, model, cost, tokens, memories created).

### Logs

```bash
pupil logs hr-assistant
pupil logs hr-assistant --follow
pupil logs hr-assistant --tail 50
```

| Flag | Description |
|---|---|
| `-f` / `--follow` | Stream logs in real time |
| `--tail <n>` | Number of lines to show (default: 100) |

### Common issues

**"No curriculum sources found"**: your `curriculum/` directory is empty and no URL sources are configured. Add files with `pupil teach` or add URLs to `pupil.yaml`.

**"API key not set"**: Pupil could not find the environment variable for your configured model. For Claude models, set `ANTHROPIC_API_KEY`. For GPT models, set `OPENAI_API_KEY`. For Gemini models, set `GOOGLE_API_KEY`. Ollama models do not need a key.

**"Container runtime not found"**: Docker or Podman is not installed or not on your `PATH`. Install one and try again.

**"Image not found"**: you tried to run or export an agent that has not been built yet. Run `pupil build` first.

**Build fails during Ollama sidecar startup**: if your host Ollama is not running, Pupil starts an Ollama sidecar container. Make sure Docker can pull the `ollama/ollama:latest` image. Alternatively, start Ollama on your host before building.

**Self-test keeps failing**: check that your test expectations are realistic. Use `pupil inspect search` to see what the agent actually learned, and adjust your curriculum or test thresholds. Also verify that each test case's `sources` field points to the correct curriculum files so remedial learning targets the right material.

## Global Configuration

```bash
pupil config list                              # Show all settings
pupil config get default_model                 # Get a specific setting
pupil config set default_model claude-haiku-4  # Set a specific setting
```

| Key | Description | Valid values |
|---|---|---|
| `default_provider` | Default LLM provider | `anthropic`, `openai`, `google`, `ollama` |
| `default_model` | Default model identifier | Any model string (e.g., `claude-sonnet-4-6`) |
| `container_runtime` | Preferred container runtime | `docker`, `podman` |
| `default_registry` | Default registry for push/pull | Any registry URL |

## Global CLI Flags

These flags work with any command:

| Flag | Description |
|---|---|
| `-v` / `--verbose` | Increase verbosity (can be repeated: `-vv`, `-vvv`) |
| `-q` / `--quiet` | Suppress non-error output |
| `--json` | Set the log format to JSON. Some commands (`inspect`, `test`, `status`, `list`, `sync`) also have their own `--json` flag that controls the output format. |
| `--color <auto\|always\|never>` | Control color output (default: `auto`) |
| `--config <path>` | Path to a config file override |

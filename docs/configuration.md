# Configuration Reference

This document covers all configuration files used by Pupil: the per-agent `pupil.yaml`, the test file `tests.yaml`, global configuration, and environment variables.

## `pupil.yaml`

The `pupil.yaml` file lives in the root of each agent directory. It defines the agent's identity, LLM model, curriculum sources, build settings, runtime behavior, and more.

All string values in `pupil.yaml` support `${VAR_NAME}` syntax for environment variable substitution. If the referenced variable is not set, loading the config will fail with an error.

### Top-level fields

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | -- | Agent name. Must match `[a-z0-9][a-z0-9-]*[a-z0-9]` and be 2-64 characters. |
| `description` | string | no | `""` | Human-readable description of the agent. |
| `model` | string | yes | -- | LLM model identifier for runtime queries (e.g., `claude-haiku-4`, `gpt-4o`, `gemini-2`). See [Model prefixes](#model-prefixes) for supported formats. |
| `learning_model` | string | no | value of `model` | LLM model identifier to use during learning. Can be a more capable model than the runtime model. |
| `fallback_model` | string | no | `null` | Fallback LLM model identifier. Used if the primary model fails or is unavailable. Runtime only. |
| `system_prompt` | string | no | `""` | System prompt sent to the LLM on every query. |
| `temperature` | float | no | `0.7` | LLM sampling temperature. Must be between 0.0 and 2.0. Runtime only. |
| `max_iterations` | int | no | `50` | Maximum number of agentic loop iterations (tool call cycles) per query. Must be at least 1. Runtime only. |
| `tool_timeout` | int | no | `30` | Timeout in seconds for individual tool calls. Runtime only. |
| `max_output_tokens` | int | no | `null` | Maximum output tokens per LLM response. If omitted, the model's default limit applies. Runtime only. |
| `mcp_servers` | map | no | `{}` | MCP servers to start inside the container. |
| `curriculum` | object | no | (defaults) | Curriculum sources and learning settings. |
| `build` | object | no | (defaults) | Build-time settings (budget, self-test). |
| `runtime` | object | no | (defaults) | Runtime settings (token limits, cost caps). |
| `routing` | object | no | `null` | Multi-agent routing configuration. |
| `pricing` | map | no | `{}` | Custom pricing overrides per model. |
| `audit` | object | no | `null` | Audit logging configuration. Runtime only. |
| `response_schema` | object | no | `null` | Structured output schema. When set, the LLM is constrained to return JSON matching the given schema. See [`response_schema`](#response_schema). Runtime only. |
| `collaboration` | object | no | `null` | Inter-agent communication configuration. When enabled, this agent can call other agents during a conversation. See [`collaboration`](#collaboration). Runtime only. |

> **Note on "Runtime only" fields:** Fields marked "Runtime only" (`fallback_model`, `temperature`, `max_iterations`, `tool_timeout`, `max_output_tokens`, `audit`, `response_schema`, `collaboration`) are recognized by the in-container agent runtime but are not parsed by CLI commands like `pupil build` or `pupil test`. You can include them in `pupil.yaml` without error, but they only take effect when the agent runs inside its container.

### Model prefixes

Pupil resolves the correct API key environment variable from the model identifier prefix:

| Prefix | Provider | API key variable |
|---|---|---|
| `claude-` or `anthropic/` | Anthropic | `ANTHROPIC_API_KEY` |
| `gpt-` or `openai/` | OpenAI | `OPENAI_API_KEY` |
| `gemini-` or `google/` | Google AI | `GOOGLE_API_KEY` |
| `vertex/` | Google Vertex AI | `VERTEX_API_KEY` |
| `ollama/` or `ollama:` | Ollama (local) | `OLLAMA_API_KEY` (optional) |
| `bedrock/` | AWS Bedrock | `AWS_ACCESS_KEY_ID` |
| `azure/` | Azure OpenAI | `AZURE_OPENAI_API_KEY` |
| (any other) | (defaults to OpenAI) | `OPENAI_API_KEY` |

### `mcp_servers`

Each key is the server name, and the value configures how to start it:

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `command` | string | yes | -- | Executable to run. |
| `args` | list of strings | no | `[]` | Command-line arguments. |
| `required` | bool | no | `false` | If `true`, the agent will not start if this server fails to connect. |
| `env` | map | no | `{}` | Environment variables to set for the server process. Values can reference host env vars with `${VAR_NAME}` syntax. |

The `recalld` MCP server is required for all agents. The minimal configuration is:

```yaml
mcp_servers:
  recalld:
    command: recalld
    args: ["mcp"]
    required: true
    env:
      RECALLD_DATA_DIR: /data/recalld
```

### `curriculum`

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `sources` | list | no | `[]` | Curriculum source entries (see below). |
| `namespace` | string | no | `"knowledge"` | recalld namespace for storing learned memories. |
| `decay` | float | no | `0.0` | Memory decay rate multiplier (0.0 = no decay). |
| `learning_profile` | string | no | `null` | Default learning profile for all sources. Must be one of: `general`, `reference`, `procedural`, `conceptual`, `faq`, `policy`, `code`. |
| `sync` | object | no | `null` | Default sync settings for URL sources. See [`curriculum.sync`](#curriculumsync-default-sync-settings). |

#### Source entries

Sources can be specified in short form (a string) or long form (an object).

**Short form:**

```yaml
curriculum:
  sources:
    - ./curriculum/                        # local directory
    - ./curriculum/handbook.md             # local file
    - https://docs.example.com/guide       # URL
```

**Long form:**

| Field | Type | Description |
|---|---|---|
| `path` | string | Path to a local file or directory (relative to the agent directory). Mutually exclusive with `url`. |
| `url` | string | URL to fetch content from. Mutually exclusive with `path`. |
| `glob` | string | Glob pattern to match files (relative to the agent directory). |
| `learning_profile` | string | Override the learning profile for this source. Must be one of: `general`, `reference`, `procedural`, `conceptual`, `faq`, `policy`, `code`. Mutually exclusive with `learning_prompt`. |
| `learning_prompt` | string | Custom learning prompt for this source. Mutually exclusive with `learning_profile`. |
| `namespace` | string | Override the recalld namespace for memories from this source. |
| `tags` | list of strings | Additional tags to apply to memories from this source. |
| `decay` | float | Override the decay rate for memories from this source. |
| `sync` | object | Per-source sync settings (for URL sources). See [Per-source sync settings](#per-source-sync-settings). |

Each source entry must have at least one of `path`, `url`, or `glob`.

```yaml
curriculum:
  sources:
    - path: ./curriculum/api-reference/
      learning_profile: reference
    - path: ./curriculum/runbooks/
      learning_profile: procedural
      tags:
        - team/sre
    - url: https://wiki.internal/eng-handbook
      sync:
        interval: 1h
    - glob: "./curriculum/**/*.md"
```

#### `curriculum.sync` (default sync settings)

These defaults apply to all URL sources unless overridden at the source level.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Whether live sync is enabled for URL sources. |
| `interval` | string | `"6h"` | How often to check for changes. Format: `Nm`, `Nh`, or `Nd` (e.g., `"30m"`, `"1h"`, `"1d"`). Minimum is `"5m"`. |
| `on_change` | string | `"in_place"` | What to do when a URL source changes. |
| `post_test` | bool | `false` | Whether to run tests after syncing changed content. |
| `test_file` | string | `"tests.yaml"` | Path to the test file to run after sync (used when `post_test` is `true`). |
| `concurrency` | int | `4` | Maximum number of concurrent HTTP requests during sync. |
| `request_delay_ms` | int | `500` | Delay in milliseconds between HTTP requests. |
| `timeout_secs` | int | `30` | HTTP request timeout in seconds. |
| `user_agent` | string | `"PupilBot/1.0 (+https://github.com/calebevans/pupil)"` | User-Agent header sent with HTTP requests. |
| `respect_robots_txt` | bool | `true` | Whether to respect `robots.txt` directives when fetching URLs. |

> **Note:** The defaults for `concurrency`, `request_delay_ms`, `timeout_secs`, and `user_agent` are applied at runtime inside the container. The CLI parses these fields but does not fill in defaults on the host side, so omitting them in `pupil.yaml` is fine.

#### Per-source sync settings

These override the default sync settings for a specific URL source.

| Field | Type | Description |
|---|---|---|
| `enabled` | bool | Override the default `sync.enabled`. |
| `interval` | string | Override the default `sync.interval`. Same format and minimum as above. |
| `on_change` | string | Override the default `sync.on_change`. Runtime only; not parsed by the CLI. |
| `strategy` | string | Sync strategy (e.g., `"confluence_api"` for Confluence pages). |
| `auth` | object | Authentication configuration. See [`auth`](#auth-authentication-for-url-sources). |
| `webhook_secret` | string | Webhook secret for push notifications. |

#### `auth` (authentication for URL sources)

| Field | Type | Description |
|---|---|---|
| `type` | string | Authentication type: `"bearer"` or `"basic"`. |
| `token` | string | Bearer token value (for `type: bearer`). Can use `${ENV_VAR}` syntax. |
| `username` | string | Username (for `type: basic`). |
| `password` | string | Password (for `type: basic`). Can use `${ENV_VAR}` syntax. |
| `headers` | map of string to string | Custom HTTP headers to include with requests. Default: `{}` (empty map). |

```yaml
sync:
  auth:
    type: bearer
    token: ${CONFLUENCE_API_TOKEN}
```

### `build`

| Field | Type | Default | Description |
|---|---|---|---|
| `max_cost_usd` | float | `null` | Maximum cost budget for the build in USD. If omitted, no budget limit is enforced. |
| `on_budget_exceeded` | string | `"abort"` | Action when the build cost exceeds `max_cost_usd`. One of: `"confirm"` (prompt the user), `"stop"`, `"continue"`, `"warn"`, or `"abort"`. |
| `self_test` | object | `null` | Self-test configuration. If omitted, no self-test cycle runs during the build. |

#### `build.self_test`

The self-test cycle runs the test suite after learning, and if the agent fails to meet the minimum score, it re-studies the sources linked to failed questions (without seeing the test answers).

| Field | Type | Default | Description |
|---|---|---|---|
| `file` | string | `"tests.yaml"` | Path to the test YAML file, relative to the agent directory. |
| `min_score` | float | `0.8` | Minimum pass rate (0.0-1.0) required for the build to succeed. |
| `max_retries` | int | `3` | Number of remedial learning cycles to attempt if the score is below `min_score`. |
| `judge_model` | string | `null` | LLM model to use for `llm_judge` assertions. If omitted, defaults to the agent's `model`. Accepts the same format as the top-level `model` field. |

> **Note:** The defaults for `file`, `min_score`, and `max_retries` are applied by the in-container runtime. When parsed by the CLI, these three fields are required if `self_test` is specified. Always provide explicit values for all three when adding `self_test` to your config.

### `runtime`

| Field | Type | Default | Description |
|---|---|---|---|
| `max_tokens_per_query` | int | `null` | Maximum tokens per query. If omitted, no per-query token limit is enforced. |
| `max_cost_per_day_usd` | float | `null` | Daily cost cap in USD. If omitted, no daily cost limit is enforced. |
| `on_budget_exceeded` | string | `"warn"` | Action when the daily cost cap is exceeded. Accepts any string value. Recognized options: `"warn"` (log a warning and continue) and `"degrade"` (switch to a cheaper model or reduce quality). Other values (such as `"confirm"`, `"stop"`, `"continue"`, `"abort"`) are accepted but have no runtime-specific behavior beyond the default. |

### `routing`

Configuration for multi-agent routing. Used when running multiple agents behind a single endpoint via `pupil router`.

| Field | Type | Default | Description |
|---|---|---|---|
| `topics` | list of strings | `[]` | Topics this agent covers. The router uses these to match incoming queries. |
| `sample_questions` | list of strings | `[]` | Example questions this agent can answer. Used by the router for classification. |
| `priority` | int | `0` | Routing priority. Lower values mean higher priority when multiple agents match. |
| `exclusive_topics` | list of strings | `[]` | Topics handled exclusively by this agent. If a query matches an exclusive topic, no other agents are considered. |

### `audit`

Configuration for audit logging of agent interactions.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Whether audit logging is enabled. |
| `log_queries` | bool | `true` | Whether to log incoming queries. |
| `log_responses` | bool | `true` | Whether to log agent responses. |
| `log_memories` | bool | `true` | Whether to log memory operations (store, recall, reinforce). |
| `redact_patterns` | list of strings | `[]` | Regex patterns for redacting sensitive data from audit logs. Matched text is replaced before writing. |
| `retention_days` | int | `90` | Number of days to retain audit log entries. |

### `response_schema`

Constrains the agent's output to a specific JSON structure. When `response_schema` is set, the LLM returns JSON that conforms to the provided JSON Schema instead of free-form text. This is useful for agents that feed their output into downstream systems that expect a predictable format.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | -- | Identifier for the schema. Must match `[a-zA-Z0-9_-]` and be at most 64 characters. |
| `description` | string | no | `null` | Human-readable description of what the schema represents. Included in the LLM request to help guide output. |
| `schema` | object | yes | -- | A JSON Schema object defining the expected response structure. When `strict` is `true`, the top-level `type` must be `"object"`. |
| `strict` | bool | no | `true` | Whether to enforce strict schema validation. When `true`, the LLM provider applies constrained decoding so the output is guaranteed to match the schema. Requires the top-level schema `type` to be `"object"`. |

The schema is passed to the LLM provider as part of each chat request. It is also injected into the system prompt so the model understands the expected output format.

```yaml
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

Per-request schema overrides are also supported via the HTTP API. See the [User Guide](user-guide.md#structured-output) for details.

### `collaboration`

Enables inter-agent communication. When enabled, the agent gains an `ask_agent` tool that lets it send questions to other agents and receive their responses during a conversation. The other agents are discovered through the router or via the `PUPIL_AGENT_REGISTRY` environment variable.

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Whether inter-agent communication is active. When `false`, the entire collaboration section is ignored. |
| `allowed_agents` | list of strings or `"all"` | `"all"` | Which agents this agent is permitted to call. Set to `"all"` to allow calling any agent in the registry, or provide a list of agent names to restrict access. |
| `max_depth` | int | `3` | Maximum call chain depth. Prevents unbounded recursion when agents call each other. A value of 3 means agent A can call agent B, which can call agent C, which can call agent D, but agent D cannot call further. |
| `timeout_secs` | int | `120` | Timeout in seconds for each inter-agent call. If the target agent does not respond within this window, the call fails with a timeout error. |
| `max_calls_per_turn` | int | `10` | Maximum number of `ask_agent` tool calls allowed per user query. Prevents runaway costs from an agent that keeps delegating. |

The `allowed_agents` field accepts two forms:

- A string `"all"`: permits calling any agent in the registry.
- A list of agent names: permits calling only those specific agents.

```yaml
# Allow calling any agent
collaboration:
  enabled: true
  allowed_agents: "all"

# Restrict to specific agents
collaboration:
  enabled: true
  allowed_agents:
    - db-expert
    - payments-bot
  max_depth: 2
  timeout_secs: 60
  max_calls_per_turn: 5
```

See the [User Guide](user-guide.md#inter-agent-communication) for a walkthrough of setting up two collaborating agents, and the [Architecture](architecture.md#10-inter-agent-communication) documentation for details on loop prevention and the call chain protocol.

### `pricing`

Custom pricing overrides for models not in the built-in pricing table, or to override built-in prices. Each key is a model identifier, and the value specifies the per-million-token cost in USD.

```yaml
pricing:
  my-custom-model:
    input_per_million: 1.50
    output_per_million: 6.00
```

| Field | Type | Description |
|---|---|---|
| `input_per_million` | float | Cost in USD per million input tokens. |
| `output_per_million` | float | Cost in USD per million output tokens. |

#### Built-in pricing table

These prices are used by default for cost tracking and budget enforcement. Override them with the `pricing` map if they are out of date.

| Model prefix | Input ($/M tokens) | Output ($/M tokens) |
|---|---|---|
| `claude-sonnet-4*` | 3.00 | 15.00 |
| `claude-opus-4*` | 15.00 | 75.00 |
| `claude-haiku*` | 0.80 | 4.00 |
| `gpt-4o-mini*` | 0.15 | 0.60 |
| `gpt-4o*` | 2.50 | 10.00 |
| `gpt-4*` | 30.00 | 60.00 |
| `gemini-1.5-flash*` | 0.075 | 0.30 |
| `gemini-1.5-pro*` | 3.50 | 10.50 |
| `gemini-2*` | 1.25 | 10.00 |
| `ollama*` | 0.00 | 0.00 |
| (any other) | 3.00 | 15.00 |

### Full example

```yaml
name: onboarding-bot
description: "Onboarding assistant for the engineering team"
model: claude-haiku-4
learning_model: claude-sonnet-4-6
fallback_model: claude-haiku-4
temperature: 0.7
max_iterations: 50
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
  web_search:
    command: /usr/local/bin/web-search-mcp
    args: []
    required: false
    env:
      API_KEY: ${WEB_SEARCH_KEY}

curriculum:
  namespace: knowledge
  decay: 0.0
  learning_profile: general
  sync:
    enabled: true
    interval: 6h
    on_change: in_place
    concurrency: 4
    request_delay_ms: 500
    timeout_secs: 30
    respect_robots_txt: true
  sources:
    - ./curriculum/
    - url: https://wiki.internal/eng-handbook
    - url: https://wiki.internal/on-call-rotation
      sync:
        interval: 1h
    - path: ./curriculum/api-reference/
      learning_profile: reference
    - path: ./curriculum/runbooks/
      learning_profile: procedural
      tags:
        - team/sre
    - url: https://confluence.internal/display/ENG/deploy-guide
      sync:
        strategy: confluence_api
        auth:
          type: bearer
          token: ${CONFLUENCE_API_TOKEN}

build:
  max_cost_usd: 10.00
  on_budget_exceeded: abort
  self_test:
    file: tests.yaml
    min_score: 0.8
    max_retries: 3

runtime:
  max_tokens_per_query: 16000
  max_cost_per_day_usd: 5.00
  on_budget_exceeded: degrade

routing:
  topics: [onboarding, dev-setup, tooling, git, ci-cd]
  exclusive_topics: []
  sample_questions:
    - "How do I set up my development environment?"
    - "What is the code review process?"
  priority: 0

audit:
  enabled: true
  log_queries: true
  log_responses: true
  log_memories: true
  redact_patterns:
    - '(?i)password\s*[:=]\s*\S+'
    - '(?i)api[_-]?key\s*[:=]\s*\S+'
  retention_days: 90

collaboration:
  enabled: true
  allowed_agents:
    - payments-bot
    - infra-bot
  max_depth: 3
  timeout_secs: 120
  max_calls_per_turn: 10

pricing:
  my-custom-model:
    input_per_million: 1.50
    output_per_million: 6.00

# response_schema:
#   name: structured_answer
#   description: "A structured answer with source attribution"
#   strict: true
#   schema:
#     type: object
#     properties:
#       answer:
#         type: string
#       sources:
#         type: array
#         items:
#           type: string
#       confidence:
#         type: number
#     required: [answer, sources, confidence]
```

---

## `tests.yaml`

The test file defines test cases for validating agent knowledge. It is used during the self-test cycle in `pupil build` and can also be run manually with `pupil test`.

### Top-level structure

A `tests.yaml` file has two top-level keys:

| Field | Type | Required | Description |
|---|---|---|---|
| `config` | object | no | Default settings for all test cases. |
| `tests` | list | yes | List of test case definitions. Must not be empty. |

### `config`

| Field | Type | Default | Description |
|---|---|---|---|
| `temperature` | float | `0.0` | LLM sampling temperature for test queries. Using `0.0` makes responses deterministic. |
| `retries` | int | `0` | Number of times to retry a failed test case before marking it as failed. |
| `timeout_secs` | int | `30` | Timeout in seconds for each test query. Must be greater than 0. |
| `judge_model` | string | `null` | LLM model for `llm_judge` assertions. If omitted, the agent's own model is used. |
| `threshold` | float | `0.8` | Default score threshold (0.0-1.0) for scored assertions like `llm_judge`, `semantic_similarity`, and `faithfulness`. A scored assertion passes when its score meets or exceeds this threshold. |

### Test case fields

Each entry in the `tests` list defines a single test case.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | -- | Unique test name. Must start with an alphanumeric character and contain only alphanumeric characters, hyphens, and underscores. |
| `description` | string | no | `null` | Human-readable description of what this test validates. |
| `question` | string | yes | -- | The question to ask the agent. Must not be empty. |
| `context` | string | no | `null` | Additional context provided to the agent alongside the question. Used as the reference text for `faithfulness` assertions if present. |
| `expects` | list | yes | -- | List of assertions to evaluate against the agent's response. Must have at least one assertion. All assertions must pass for the test to pass. |
| `sources` | list of strings | no | `[]` | Curriculum source paths linked to this test. During the self-test cycle, if this test fails, the agent re-studies these specific sources. |
| `threshold` | float | no | `null` | Per-test pass threshold (0.0-1.0), overriding `config.threshold`. |
| `tags` | list of strings | no | `[]` | Tags for filtering and organizing tests (e.g., `["critical", "api"]`). |

### Assertion types

Each assertion is a single-key map. The key is the assertion type and the value is its parameter. String comparisons for `contains`, `not_contains`, `contains_any`, `contains_all`, and `starts_with` are case-insensitive. Regex assertions (`matches`, `not_matches`) are case-sensitive.

#### `contains`

Passes if the response contains the given substring.

```yaml
expects:
  - contains: "npm install"
```

#### `not_contains`

Passes if the response does not contain the given substring.

```yaml
expects:
  - not_contains: "pip install"
```

#### `contains_any`

Passes if the response contains at least one of the given substrings. The list must not be empty.

```yaml
expects:
  - contains_any: ["PostgreSQL", "Postgres", "psql"]
```

#### `contains_all`

Passes if the response contains all of the given substrings. The list must not be empty.

```yaml
expects:
  - contains_all: ["email", "name", "password"]
```

#### `matches`

Passes if the response matches the given regular expression.

```yaml
expects:
  - matches: "\\b(required|mandatory)\\b"
```

#### `not_matches`

Passes if the response does not match the given regular expression.

```yaml
expects:
  - not_matches: "\\d{3}-\\d{2}-\\d{4}"   # no SSN-like patterns
```

#### `starts_with`

Passes if the response starts with the given string.

```yaml
expects:
  - starts_with: "To set up your"
```

#### `llm_judge`

Uses an LLM to evaluate the response against a natural-language criteria string. This is useful for evaluating free-form answers where exact string matching is insufficient.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `criteria` | string | yes | -- | Natural-language description of what a correct response should contain or demonstrate. Must not be empty. |
| `threshold` | float | no | `null` | Score threshold (0.0-1.0) for this specific judgment. Overrides the test-level and config-level thresholds. |

```yaml
expects:
  - llm_judge:
      criteria: "Response describes a multi-step deployment process including a PR merge, CI pipeline, and staging environment verification."
  - llm_judge:
      criteria: "Response acknowledges uncertainty rather than fabricating an answer."
      threshold: 0.9
```

#### `memory_hit`

Passes if the agent called `recall_memories` and received results (when `true`), or did not call it or received no results (when `false`).

```yaml
expects:
  - memory_hit: true
```

#### `memory_source`

Passes if one of the recalled memories has a `source/` tag matching the given value.

```yaml
expects:
  - memory_source: "handbook.md"
```

#### `memory_query`

Passes if the query string passed to `recall_memories` contains the given substring (case-insensitive).

```yaml
expects:
  - memory_query: "deployment"
```

#### `tool_called`

Passes if the agent called the named tool at least once during the query.

```yaml
expects:
  - tool_called: "web_search"
```

#### `tool_not_called`

Passes if the agent did not call the named tool during the query.

```yaml
expects:
  - tool_not_called: "web_search"
```

#### `latency_ms`

Passes if the total response latency is at or below the specified maximum in milliseconds. The `max` value must be greater than 0.

| Field | Type | Required | Description |
|---|---|---|---|
| `max` | int | yes | Maximum allowed latency in milliseconds. |

```yaml
expects:
  - latency_ms:
      max: 10000
```

#### `token_count`

Passes if the total token count (input + output) is at or below the specified maximum. The `max` value must be greater than 0.

| Field | Type | Required | Description |
|---|---|---|---|
| `max` | int | yes | Maximum allowed total tokens. |

```yaml
expects:
  - token_count:
      max: 5000
```

#### `semantic_similarity`

Uses embeddings to compare the agent's response against a reference text. Passes if the cosine similarity score meets or exceeds the threshold.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `reference` | string | yes | -- | Reference text to compare against. Must not be empty. |
| `threshold` | float | no | `null` | Score threshold (0.0-1.0). Overrides the test-level and config-level thresholds. |

```yaml
expects:
  - semantic_similarity:
      reference: "To update your feature branch, rebase it onto main."
      threshold: 0.7
```

#### `faithfulness`

Decomposes the agent's response into atomic claims and checks each claim against the available context. The context is taken from the test case's `context` field if present, or from recalled memories if not. Passes if the fraction of supported claims meets or exceeds the threshold.

| Field | Type | Required | Default | Description |
|---|---|---|---|---|
| `threshold` | float | no | `null` | Score threshold (0.0-1.0). Overrides the test-level and config-level thresholds. |

```yaml
expects:
  - faithfulness:
      threshold: 0.9
```

### Full example

```yaml
config:
  temperature: 0
  retries: 2
  timeout_secs: 30
  judge_model: claude-haiku-4
  threshold: 0.8

tests:
  - name: dev-environment-setup
    question: "How do I set up my development environment?"
    sources:
      - ./curriculum/setup-guide.md
    expects:
      - contains: "clone the repo"
      - contains: "npm install"
      - not_contains: "pip install"

  - name: on-call-lead
    question: "Who is the on-call lead for payments?"
    expects:
      - contains_any: ["Alice", "Alice Chen"]

  - name: deploy-process-staging
    question: "What is the deploy process for staging?"
    sources:
      - ./curriculum/handbook.md
    expects:
      - llm_judge:
          criteria: "Response describes a multi-step deployment process including a PR merge, CI pipeline, and staging environment verification."

  - name: no-hallucination-on-unknown
    question: "What is the company's policy on bringing pets to the office?"
    expects:
      - llm_judge:
          criteria: "Response acknowledges that it does not have information about this topic rather than fabricating an answer."

  - name: factual-grounding
    question: "What database does the payments service use?"
    context: "The payments service uses PostgreSQL 15 with read replicas."
    expects:
      - contains_any: ["PostgreSQL", "Postgres"]

  - name: api-endpoint-details
    question: "What are the required parameters for POST /api/v2/users?"
    expects:
      - contains_all: ["email", "name"]
      - matches: "\\b(required|mandatory)\\b"

  - name: semantic-accuracy
    question: "How do I update my feature branch?"
    tags: ["git", "workflow"]
    expects:
      - starts_with: "To update"
      - contains: "rebase"
```

---

## Global configuration

Global settings are stored in a platform-appropriate config directory. The file path is:

- **Linux:** `~/.config/pupil/config.yaml`
- **macOS:** `~/Library/Application Support/pupil/config.yaml`
- **Windows:** `%APPDATA%\pupil\config.yaml`

Manage global settings with `pupil config`:

```bash
pupil config list                            # show all settings
pupil config get default_model               # get a single value
pupil config set default_model claude-haiku-4 # set a value
```

### Available keys

| Key | Type | Valid values | Description |
|---|---|---|---|
| `default_provider` | string | any provider name (e.g. `anthropic`, `openai`, `google`, `ollama`) | Default LLM provider for new agents created by `pupil create`. |
| `default_model` | string | any model identifier | Default model for new agents. Used by `pupil create` when no `--model` flag is given. |
| `container_runtime` | string | `docker`, `podman` | Override container runtime auto-detection. If not set, Pupil checks for `docker` first, then `podman`. |
| `default_registry` | string | any registry URL (e.g. `ghcr.io/myorg`) | Default OCI registry for `pupil push` and `pupil pull` operations. |

---

## Environment variables

### LLM API keys

| Variable | Provider |
|---|---|
| `ANTHROPIC_API_KEY` | Anthropic (Claude models) |
| `OPENAI_API_KEY` | OpenAI (GPT models) |
| `GOOGLE_API_KEY` | Google AI (Gemini models) |
| `VERTEX_API_KEY` | Google Vertex AI |
| `VERTEX_PROJECT_ID` | Google Vertex AI project ID |
| `VERTEX_LOCATION` | Google Vertex AI region |
| `AWS_ACCESS_KEY_ID` | AWS Bedrock |
| `AWS_SECRET_ACCESS_KEY` | AWS Bedrock |
| `AWS_SESSION_TOKEN` | AWS Bedrock (optional session token) |
| `AWS_REGION` / `AWS_DEFAULT_REGION` | AWS Bedrock region |
| `AZURE_OPENAI_API_KEY` | Azure OpenAI |
| `AZURE_OPENAI_ENDPOINT` | Azure OpenAI endpoint URL |

### Ollama

| Variable | Default | Description |
|---|---|---|
| `OLLAMA_BASE_URL` | `http://localhost:11434` | Ollama API endpoint on the host. |
| `OLLAMA_API_KEY` | (none) | API key for Ollama, if your instance requires authentication. Usually not needed. |

### Pupil internals

| Variable | Description |
|---|---|
| `PUPIL_CONFIG` | Override the path to `pupil.yaml` inside the container. Default: `/agent/pupil.yaml`. |
| `PUPIL_SKIP_SETUP` | Set to `1` to skip the first-run setup wizard. |
| `PUPIL_LOG_FORMAT` | Log output format. Set to `json` for structured logging (used automatically during builds). Default: `human`. |
| `PUPIL_JUDGE_MODEL` | Model identifier for the LLM judge. Set automatically during `pupil build` from `build.self_test.judge_model`. |
| `PUPIL_AGENT_REGISTRY` | JSON array of agent registry entries. Set automatically by `pupil router` when starting agents with collaboration enabled. Each entry is an object with `name`, `url`, and optional `description`. Example: `[{"name":"db-expert","url":"http://db:8080","description":"Database expert"}]`. |
| `PUPIL_ROUTER_URL` | URL of the Pupil router. When set, inter-agent calls are routed through the router instead of directly to the target agent. Example: `http://router:9090`. |

### recalld (passed to the container during build and run)

| Variable | Description |
|---|---|
| `RECALLD_DATA_DIR` | Path to recalld's data directory inside the container. |
| `RECALLD_EMBEDDING_PROVIDER` | Embedding provider (always `ollama` for Pupil). |
| `RECALLD_EMBEDDING_MODEL` | Embedding model name. Default: `embeddinggemma:latest`. |
| `RECALLD_EMBEDDING_BASE_URL` | Ollama URL as seen from inside the container (typically `http://host.docker.internal:11434`). |
| `RECALLD_EMBEDDING_DIMENSIONS` | Embedding vector dimensions. Default: `768`. |

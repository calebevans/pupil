# Testing

Pupil includes a testing framework for validating that your agent learned its curriculum correctly. You define test cases in a YAML file, each with a question and a set of assertions about the expected response. Tests run in two contexts: standalone via `pupil test`, and automatically during `pupil build` when self-test is configured.

## Overview

The testing framework asks your agent questions and checks the responses against assertions you define. It covers three concerns:

1. **Content correctness.** Does the agent's response contain the right information? String assertions (`contains`, `matches`, etc.) handle deterministic checks. `llm_judge` handles free-form evaluation where exact wording varies.

2. **Retrieval behavior.** Did the agent use its memory correctly? Retrieval assertions (`memory_hit`, `memory_source`, `tool_called`) verify that the agent recalled the right memories or called the right tools.

3. **Operational bounds.** Did the agent respond within acceptable latency and token budgets? `latency_ms` and `token_count` enforce performance constraints.

Tests run inside the agent's container. The CLI starts the container, sends the test file to the in-container test runner, collects JSON results, and reports them.

For a complete working example, see [`examples/astronomy-agent/tests.yaml`](../examples/astronomy-agent/tests.yaml). The same directory also includes `benchmark/mmlu-astronomy.yaml` with 152 MMLU questions for standardized evaluation.

## Writing tests

Create a file called `tests.yaml` in your agent directory. The file has two top-level keys: `config` (optional defaults) and `tests` (the test cases).

```yaml
config:
  temperature: 0
  retries: 1
  timeout_secs: 30
  threshold: 0.8

tests:
  - name: knows-deploy-process
    question: "How do we deploy to staging?"
    sources:
      - curriculum/deploy-guide.md
    expects:
      - memory_hit: true
      - contains: "staging"
      - llm_judge:
          criteria: "The response describes a multi-step deployment process"
```

### Test case fields

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Unique identifier. Must start with an alphanumeric character and contain only alphanumerics, hyphens, and underscores. |
| `description` | string | no | Human-readable description of what the test verifies. |
| `question` | string | yes | The question to ask the agent. Must not be empty. |
| `context` | string | no | Additional context provided to the agent alongside the question. Also used as the reference text for `faithfulness` assertions. |
| `tags` | list of strings | no | Tags for filtering tests with `--filter`. |
| `sources` | list of strings | no | Source files this question relates to. Used by the self-test remedial cycle to target re-learning on failure. |
| `threshold` | float | no | Override the config-level threshold for this test case (0.0 to 1.0). |
| `expects` | list of assertions | yes | At least one assertion. All assertions must pass for the test to pass. |

### Naming conventions

Test names should be descriptive and use kebab-case:

```yaml
- name: knows-database-choice
- name: explains-ci-pipeline
- name: rejects-out-of-scope-question
```

Names must match the pattern `^[a-zA-Z0-9][a-zA-Z0-9_-]*$`. Duplicate names within a file cause a validation error.

## Assertion types

Pupil supports 17 assertion types, organized into four categories by cost (cheapest to most expensive). Within a test case, assertions are evaluated in priority order so that cheap checks fail fast before incurring LLM calls.

All 17 assertion types are fully evaluated when you run `pupil test` on the host. During `pupil build` self-test, only string assertions (`contains`, `not_contains`, `contains_any`, `contains_all`, `matches`, `not_matches`, `starts_with`) and `llm_judge` are evaluated inside the container. Retrieval assertions (`memory_hit`, `memory_source`, `memory_query`, `tool_called`, `tool_not_called`), operational assertions (`latency_ms`, `token_count`), and other semantic assertions (`semantic_similarity`, `faithfulness`) are silently skipped (treated as passing) during in-container self-test.

### String assertions

These check the response text directly. Substring comparisons (`contains`, `not_contains`, `contains_any`, `contains_all`, `starts_with`) are case-insensitive. The regex assertions (`matches`, `not_matches`) are case-sensitive by default; include `(?i)` in your regex pattern to make them case-insensitive.

#### `contains`

Passes if the response contains the given substring.

```yaml
- contains: "PostgreSQL"
```

#### `not_contains`

Passes if the response does NOT contain the given substring.

```yaml
- not_contains: "MySQL"
```

#### `contains_any`

Passes if the response contains at least one of the given substrings. The list must not be empty.

```yaml
- contains_any:
    - "PostgreSQL"
    - "Postgres"
    - "psql"
```

#### `contains_all`

Passes if the response contains ALL of the given substrings. The list must not be empty.

```yaml
- contains_all:
    - "clone the repo"
    - "npm install"
    - "npm test"
```

#### `matches`

Passes if the response matches the given regular expression. The pattern is validated at parse time, so invalid regex causes a validation error before any tests run.

```yaml
- matches: "v\\d+\\.\\d+\\.\\d+"
```

#### `not_matches`

Passes if the response does NOT match the given regular expression.

```yaml
- not_matches: "\\b(TODO|FIXME)\\b"
```

#### `starts_with`

Passes if the response starts with the given prefix. Leading whitespace in the response is trimmed before comparison. The comparison is case-insensitive.

```yaml
- starts_with: "To deploy"
```

### Retrieval assertions

These check how the agent interacted with its memory tools during the response.

#### `memory_hit`

Passes if the agent called `recall_memories` and got results (when `true`) or did not call it / got no results (when `false`).

```yaml
- memory_hit: true    # agent should recall relevant memories
- memory_hit: false   # agent should not find relevant memories
```

Use `memory_hit: false` for negative tests where the agent should not have knowledge about the topic.

#### `memory_source`

Passes if the recalled memories include one from the specified source file. Source matching looks at the `source/` tag on each memory.

```yaml
- memory_source: "deploy-guide.md"
```

#### `memory_query`

Passes if the agent's `recall_memories` query contains the given substring (case-insensitive).

```yaml
- memory_query: "deploy"
```

#### `tool_called`

Passes if the agent called the named tool during the response.

```yaml
- tool_called: "recall_memories"
```

#### `tool_not_called`

Passes if the agent did NOT call the named tool.

```yaml
- tool_not_called: "web_search"
```

### Operational assertions

These check response performance characteristics.

#### `latency_ms`

Passes if the total response latency is at or below the given maximum (in milliseconds). The `max` value must be greater than 0.

```yaml
- latency_ms:
    max: 10000
```

#### `token_count`

Passes if the total token count (input + output) is at or below the given maximum. The `max` value must be greater than 0.

```yaml
- token_count:
    max: 5000
```

### Semantic assertions

These use embeddings or LLMs to evaluate response quality. They are the most expensive assertions because they require additional model calls.

#### `semantic_similarity`

Passes if the response is semantically similar to the given reference text, above the threshold. Similarity is computed via cosine similarity of embeddings. The `reference` field must not be empty.

If `threshold` is omitted, it falls back to the test-level threshold, then the config-level threshold (default 0.8).

```yaml
- semantic_similarity:
    reference: "Deploy to staging by merging to the staging branch"
    threshold: 0.7
```

#### `llm_judge`

Uses an LLM to evaluate the response against the given criteria. The LLM scores the response from 0.0 to 1.0. The `criteria` field must not be empty.

```yaml
- llm_judge:
    criteria: "The response accurately describes the code review process, including PR creation, reviewer assignment, and merge requirements"
    threshold: 0.8
```

See the [LLM Judge](#llm-judge) section below for details on how scoring works.

#### `faithfulness`

Uses an LLM to check whether the response is faithful to the retrieved context. The evaluation decomposes the response into atomic claims and checks each claim against the context.

Context is resolved in this order:

1. The `context` field on the test case, if present.
2. The memories recalled during the response, if any.
3. Fails if neither is available.

If `threshold` is omitted, it falls back to the test-level threshold, then the config-level threshold (default 0.8).

```yaml
- faithfulness:
    threshold: 0.8
```

## LLM judge

The `llm_judge` assertion sends the question, the agent's response, and your evaluation criteria to a judge LLM. The judge returns a score from 0.0 to 1.0 along with a brief reasoning.

### Scoring prompt

The judge LLM receives a prompt with scoring guidelines. The exact prompt differs depending on where tests run.

During `pupil build` self-test (in-container), the judge prompt uses these anchors:

- **1.0**: fully satisfies every criterion with accurate, complete information.
- **0.7**: mostly satisfies the criteria with minor gaps or imprecisions.
- **0.4**: partially addresses the criteria but has significant gaps.
- **0.0**: completely fails to address the criteria or provides wrong information.

The in-container prompt also instructs the judge to be strict (vague or generic responses score below 0.5) but fair (the response does not need to use the exact same words as the criteria, only convey the same concepts).

During `pupil test` (host-side), the judge prompt uses a simpler scale: "1.0 means the response fully satisfies all criteria, 0.0 means the response completely fails the criteria, use intermediate values for partial matches." It does not include the 0.7 and 0.4 anchors.

In both cases, the judge returns a JSON object `{"score": <float>, "reasoning": "<explanation>"}`. The parser handles code fences and surrounding text.

### Threshold resolution

The threshold for `llm_judge` is resolved from most specific to least specific:

1. `threshold` on the `llm_judge` assertion itself
2. `threshold` on the test case
3. `threshold` in the `config` section (default: 0.8)

For example:

```yaml
config:
  threshold: 0.7          # global default

tests:
  - name: basic-question
    question: "What database do we use?"
    threshold: 0.8         # overrides config for this test
    expects:
      - llm_judge:
          criteria: "Mentions PostgreSQL"
          threshold: 0.9   # overrides both test and config for this assertion
```

### Configuring a judge model

By default, the agent's own model evaluates `llm_judge` and `faithfulness` assertions. You can specify a different model for judging at several levels:

In `tests.yaml`:

```yaml
config:
  judge_model: "claude-sonnet-4-6"
```

In `pupil.yaml` (for self-test during build):

```yaml
build:
  self_test:
    file: tests.yaml
    min_score: 0.8
    max_retries: 3
    judge_model: "claude-sonnet-4-6"
```

The `pupil test` CLI does not have a `--judge-model` flag. To use a different judge model, set it in `tests.yaml` or `pupil.yaml` as shown above. The test file config overrides the agent model. Using a separate judge model is useful when your agent runs a smaller or cheaper model but you want a more capable model to evaluate its responses.

## Self-test during build

When `build.self_test` is configured in `pupil.yaml`, the build process includes an automated test-learn cycle after the initial learning phase.

### Configuration

```yaml
build:
  self_test:
    file: tests.yaml
    min_score: 0.8
    max_retries: 3
    judge_model: claude-sonnet-4-6   # optional
```

| Field | Type | Description |
|---|---|---|
| `file` | string | Path to the test file, relative to the agent directory. |
| `min_score` | float | Minimum pass rate (0.0 to 1.0) required for the build to succeed. |
| `max_retries` | integer | Maximum number of remedial learning cycles before the build fails. |
| `judge_model` | string | Optional model for `llm_judge` and `faithfulness` assertions. Defaults to the agent's own model. |

### How the cycle works

1. The agent learns all curriculum sources.
2. The test suite runs.
3. If the pass rate meets `min_score`, the build proceeds to commit the image.
4. If the pass rate is below `min_score`, the build identifies which source files are linked to failing tests (via the `sources` field on each test case).
5. The agent re-reads those specific source files with a remedial prompt that focuses on finding missed details.
6. The test suite runs again.
7. Steps 4-6 repeat up to `max_retries` times.

If the pass rate is still below `min_score` after all retries, the build fails with an error listing the pass rate, threshold, and the source files that were targeted.

### Remedial learning

The remedial prompt instructs the agent to:

- Re-read the specific source documents linked to failed tests.
- Use `recall_memories` to check what it already knows from those documents.
- Look for gaps, missing details, and incorrect information.
- Store additional insights without duplicating existing memories (using `find_similar_memories` to check first).

The remedial prompt does NOT include the test questions or answers. The agent only re-reads the source documents. This ensures the agent learns the material rather than memorizing test answers.

Each remedial cycle re-learns with `--force-relearn` so the agent processes the source documents again regardless of content hash.

## Source-linked questions

The `sources` field on a test case connects the question to the curriculum files that contain its answer. This serves two purposes:

1. **Targeted remedial learning.** When a test fails during `pupil build`, only the linked sources are re-read instead of the entire curriculum. This makes remedial cycles faster and more focused.

2. **Documentation.** It records which source documents each test validates, making your test suite easier to maintain.

```yaml
tests:
  - name: knows-api-rate-limits
    question: "What are our API rate limits?"
    sources:
      - curriculum/api-reference.md
    expects:
      - contains: "rate limit"
      - llm_judge:
          criteria: "The response includes specific rate limit numbers"

  - name: knows-incident-process
    question: "What should I do when a P1 incident occurs?"
    sources:
      - curriculum/incident-runbook.md
      - curriculum/on-call-guide.md
    expects:
      - contains_all:
          - "page"
          - "incident channel"
      - llm_judge:
          criteria: "Describes who to contact and what steps to follow"
```

When `knows-api-rate-limits` fails during build, the remedial cycle re-reads only `curriculum/api-reference.md`. When `knows-incident-process` fails, both `curriculum/incident-runbook.md` and `curriculum/on-call-guide.md` are re-read.

If a failing test has no `sources` field, it does not contribute to targeted re-learning. The remedial cycle still runs, but it cannot narrow down which documents to revisit for that specific failure.

## Running tests standalone

Use `pupil test` to run tests against a built agent image.

```bash
pupil test                             # runs tests.yaml in current directory
pupil test --file my-tests.yaml        # specify test file
pupil test --filter "deploy*"          # filter by name or tag glob
pupil test --json                      # JSON output to stdout
pupil test --junit results.xml         # JUnit XML output
pupil test --show-details              # show detailed assertion results
pupil test --threshold 0.9             # override pass/fail threshold
pupil test --retries 2                 # override retries per test
pupil test --timeout 60                # override timeout in seconds
```

### Command-line options

| Flag | Description |
|---|---|
| `--file <path>` | Path to the test file (default: `tests.yaml`). |
| `--filter <glob>` | Filter tests by name or tag. Supports `*` wildcards and substring matching. |
| `--json` | Output results as JSON to stdout. |
| `--junit <path>` | Write JUnit XML results to the given path. |
| `--show-details` | Show detailed assertion results for each test. |
| `--threshold <float>` | Override the config threshold. |
| `--retries <int>` | Override the config retries. |
| `--timeout <int>` | Override the config timeout (seconds). |

### Filtering

The `--filter` flag matches against test names and tags using glob patterns:

```bash
pupil test --filter "deploy*"          # all tests whose name starts with "deploy"
pupil test --filter "*staging*"        # all tests with "staging" anywhere in the name
pupil test --filter "deploy"           # substring match (no wildcards needed)
```

Filtering is case-insensitive. If no tests match the filter, the command exits with code 2.

### Exit codes

- `0`: all tests passed.
- `1`: one or more tests failed.
- `2`: validation error (invalid test file, no matching filter, etc.).

### JSON output

With `--json`, the output includes a summary and per-test results:

```json
{
  "agent": "my-agent",
  "timestamp": "1719360000",
  "summary": {
    "total": 5,
    "passed": 4,
    "failed": 1,
    "pass_rate": 0.8,
    "total_latency_ms": 12500,
    "total_input_tokens": 8200,
    "total_output_tokens": 3100
  },
  "tests": [
    {
      "name": "knows-deploy-process",
      "question": "How do we deploy to staging?",
      "response": "To deploy to staging, merge your PR...",
      "assertions": [
        {
          "assertion_type": "contains",
          "passed": true,
          "score": null,
          "threshold": null,
          "detail": "Found 'staging' in response"
        }
      ],
      "latency_ms": 2500,
      "input_tokens": 1640,
      "output_tokens": 620,
      "tool_calls": [],
      "retries_used": 0,
      "passed": true
    }
  ]
}
```

## Generating tests

Pupil can generate test cases from your curriculum:

```bash
pupil test --generate
pupil test --generate --count 20
pupil test --generate --output tests.yaml
```

This reads the curriculum sources, sends them to the LLM, and generates test cases with appropriate assertion types. Review and edit the generated tests before using them.

| Flag | Description |
|---|---|
| `--generate` | Enable test generation mode. |
| `--count <int>` | Number of test cases to generate (default: 10). |
| `--output <path>` | Write generated tests to a file (otherwise prints to stdout). |

## Test configuration

The `config` section sets defaults for all tests in the file.

| Field | Type | Default | Description |
|---|---|---|---|
| `temperature` | float | `0.0` | LLM temperature for test queries. Use 0 for deterministic results. |
| `retries` | integer | `0` | Number of retries per test case. A test with `retries: 2` runs up to 3 times total (1 initial + 2 retries). The test passes if any attempt succeeds. |
| `timeout_secs` | integer | `30` | Timeout per test case in seconds. Must be greater than 0. |
| `judge_model` | string | `null` | LLM model for `llm_judge` and `faithfulness` assertions. Defaults to the agent's own model. |
| `threshold` | float | `0.8` | Default score threshold (0.0-1.0) for scored assertions like `llm_judge`, `semantic_similarity`, and `faithfulness`. A scored assertion passes when its score meets or exceeds this threshold. |

## Test YAML validation

The test file is validated before execution. Validation checks include:

- At least one test case is defined.
- No duplicate test names.
- Test names match `^[a-zA-Z0-9][a-zA-Z0-9_-]*$`.
- Questions are not empty.
- Each test has at least one assertion.
- Thresholds are between 0.0 and 1.0 (config, test, and assertion levels).
- `timeout_secs` is greater than 0.
- Regex patterns in `matches` / `not_matches` are valid.
- `contains_any` and `contains_all` lists are not empty.
- `latency_ms.max` and `token_count.max` are greater than 0.
- `llm_judge.criteria` is not empty.
- `semantic_similarity.reference` is not empty.

If validation fails, the command prints the errors and exits with code 2.

## Tips for writing good tests

**Use string assertions for deterministic facts.** If the answer must include a specific term (a database name, a tool, a version number), `contains` is cheaper and more reliable than `llm_judge`.

```yaml
- name: knows-database
  question: "What database does the payments service use?"
  sources:
    - curriculum/architecture.md
  expects:
    - contains: "PostgreSQL"
    - memory_hit: true
```

**Use `llm_judge` for conceptual understanding.** When the answer could be phrased many ways, write clear criteria describing what the response should convey.

```yaml
- name: explains-caching-strategy
  question: "Why do we use Redis for session storage?"
  sources:
    - curriculum/architecture.md
  expects:
    - llm_judge:
        criteria: "Explains that Redis provides fast in-memory access and supports TTL expiration for sessions"
```

**Include negative tests.** Verify that the agent admits uncertainty for topics outside its curriculum rather than hallucinating.

```yaml
- name: rejects-unknown-topic
  question: "What is our policy on bringing pets to the office?"
  expects:
    - memory_hit: false
    - llm_judge:
        criteria: "The agent states that it does not have information about this topic rather than guessing"
```

**Link tests to sources.** Always populate the `sources` field so the self-test remedial cycle can target re-learning effectively.

**Combine assertion types.** Use cheap string assertions alongside `llm_judge` to get both fast deterministic checks and nuanced evaluation.

```yaml
- name: knows-incident-response
  question: "What do I do during a P1 incident?"
  sources:
    - curriculum/incident-runbook.md
  expects:
    - memory_hit: true
    - contains_any:
        - "PagerDuty"
        - "page"
    - contains: "incident channel"
    - llm_judge:
        criteria: "Describes the escalation process including who to notify and the expected response timeline"
```

**Set temperature to 0 for reproducibility.** The default `temperature: 0` in the config section produces more consistent results across test runs.

**Use tags for organization.** Tags let you run subsets of your test suite:

```yaml
tests:
  - name: knows-deploy-process
    tags: [deployment, critical]
    question: "How do we deploy to staging?"
    expects:
      - contains: "staging"

  - name: knows-rollback
    tags: [deployment, critical]
    question: "How do we roll back a bad deploy?"
    expects:
      - contains: "rollback"
```

```bash
pupil test --filter "deployment"       # run only deployment tests
pupil test --filter "critical"         # run only critical tests
```

**Keep criteria specific.** Vague `llm_judge` criteria like "the response is good" will produce inconsistent scores. Specify what facts or concepts the response should include.

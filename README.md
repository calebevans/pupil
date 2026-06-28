# Pupil

Pupil is a CLI tool for building teachable AI agents that carry their knowledge as OCI container images. You define an agent with a `pupil.yaml` config file, give it curriculum (documents, URLs), build it into a container image, and distribute it through any OCI registry. The agent's knowledge lives inside the container. When you run the agent, it uses its learned memories to answer questions. When you push the image to a registry, the knowledge travels with it.

## Prerequisites

- **Rust 1.85+** (for building from source)
- **Docker** or **Podman**
- An **LLM API key** for at least one supported provider (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GOOGLE_API_KEY`)
- **Embeddings**: recalld needs an embedding provider. By default, Pupil uses Ollama with `embeddinggemma`. If Ollama is running on your host, the build connects to it automatically. If not, the build starts an Ollama sidecar container (no local install needed). You can also configure recalld to use a different embedding provider.

Run `pupil doctor` after installation to verify your environment.

## Installation

```bash
git clone https://github.com/calebevans/pupil.git
cd pupil
cargo install --path crates/pupil-cli
```

Then build the base container image that all agents derive from:

```bash
docker build -t pupil-base:dev -f container/Dockerfile .
```

## Quick start

```bash
# 1. Create a new agent project
pupil create my-agent
cd my-agent

# 2. Add curriculum content (local files or URLs)
pupil teach /path/to/docs/
pupil teach --url https://docs.example.com/guide

# 3. Build the agent (the LLM reads and learns the curriculum)
export ANTHROPIC_API_KEY="sk-..."
pupil build

# 4. Chat with the agent interactively
pupil run

# 5. Or run it as an HTTP server
pupil run --port 8080

# 6. Push the image to a registry
pupil push registry.example.com/my-agent:v1
```

`pupil create` accepts a `--template` flag. Available templates: `minimal` (default), `full`, `knowledge-base`, `chatbot`. You can also set the model with `--model` (defaults to `claude-sonnet-4-6`).

## How it works

Pupil's learning process is agentic. During `pupil build`, an LLM reads your curriculum content, comprehends it, and decides what to store as memories in [recalld](https://github.com/calebevans/recalld) (an MCP memory server running inside the container). The agent reads the content, understands it, and organizes the knowledge accordingly.

If you define tests in a `tests.yaml` file, the build includes a self-test cycle. After learning, the agent is tested against your questions. If it fails to meet the minimum pass rate, it re-studies the specific source documents linked to the failed questions (without seeing the test answers). This loop repeats until the agent passes or exhausts its retry budget.

After learning completes, `docker commit` snapshots the container filesystem (including the recalld database) into an OCI image. The result is a self-contained agent image that you can run anywhere containers run.

```
pupil create  -->  pupil teach  -->  pupil build  -->  pupil run
     |                  |                 |                |
  Scaffolds        Adds files/      LLM reads &      Starts the
  pupil.yaml       URLs to the      learns the        agent in a
  + curriculum/    curriculum/      curriculum,        container
                   directory        snapshots image    (chat or HTTP)
```

## CLI commands

| Command | Description |
|---------|-------------|
| `pupil create <name>` | Create a new agent directory with `pupil.yaml` and `curriculum/` |
| `pupil teach <paths...>` | Add content to an agent's curriculum |
| `pupil build` | Build an agent: learn the curriculum and snapshot the image |
| `pupil run` | Run an agent: start the container and begin chatting |
| `pupil test` | Run tests against a built agent |
| `pupil push <registry>` | Push an agent image to an OCI registry |
| `pupil pull <registry>` | Pull an agent image from an OCI registry |
| `pupil list` | List locally registered agents |
| `pupil export` | Export an agent as an OCI archive tar file |
| `pupil import` | Import an agent from an OCI archive tar file |
| `pupil status` | Show agent status, build info, and runtime usage |
| `pupil logs` | Show logs from a running agent container |
| `pupil doctor` | Validate the environment: container runtime, API keys, Ollama |
| `pupil config` | Get, set, or list global configuration values |
| `pupil inspect` | Inspect learned memories: list, search, stats, quality, graph, diff |
| `pupil watch` | Watch curriculum for changes and re-learn automatically |
| `pupil commit` | Snapshot runtime volume state into a new image |
| `pupil sync` | Check URL sources for changes and re-learn |
| `pupil router` | Manage the multi-agent router |
| `pupil completions` | Generate shell completion scripts |

## Supported LLM providers

Pupil uses the [genai](https://crates.io/crates/genai) crate and supports any provider it supports:

- Anthropic (Claude)
- OpenAI (GPT)
- Google (Gemini)
- Ollama (local models)
- AWS Bedrock
- Google Vertex AI
- Azure OpenAI

Set the appropriate API key environment variable for your chosen model (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GOOGLE_API_KEY`).

## Example

The [`examples/astronomy-agent`](examples/astronomy-agent) directory contains a complete working agent that learns introductory astronomy from the [OpenStax Astronomy 2e](https://openstax.org/details/books/astronomy-2e/) textbook (CC-BY 4.0). It uses 30 full chapter URLs as curriculum and includes a self-test with 29 questions for the build cycle. A separate `benchmark/mmlu-astronomy.yaml` file contains 152 questions from the [MMLU benchmark](https://huggingface.co/datasets/cais/mmlu) for standardized evaluation after the build.

```bash
cd examples/astronomy-agent
pupil build
pupil run

# Run the MMLU benchmark against the built agent
pupil test --file benchmark/mmlu-astronomy.yaml
```

After learning the textbook, the agent scores **142/152 (93.4%)** on the MMLU Astronomy benchmark. See [Benchmark Results](docs/benchmark.md) for methodology, reproduction steps, and limitations.

## Documentation

- [User Guide](docs/user-guide.md) -- getting started, full workflow, all commands
- [Configuration Reference](docs/configuration.md) -- `pupil.yaml`, `tests.yaml`, global config, environment variables
- [Architecture](docs/architecture.md) -- two-crate design, container strategy, learning pipeline
- [Testing](docs/testing.md) -- writing tests, assertion types, self-test during build
- [Benchmark Results](docs/benchmark.md) -- MMLU Astronomy evaluation, methodology, reproduction
- [Glossary](docs/GLOSSARY.md) -- terminology and definitions

## License

AGPL-3.0. See [LICENSE](LICENSE) for details.

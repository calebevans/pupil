# PhantomWiki Benchmark

## Overview

This document reports the results of running the [PhantomWiki](https://arxiv.org/abs/2502.20377) benchmark against a Pupil agent. PhantomWiki (ICML 2025, Gong et al., Cornell/Cambridge) generates fictional wiki articles about made-up characters with family relationships, friendships, occupations, hobbies, and dates of birth. Questions test whether systems can answer from learned content, not from training data. Since the characters are entirely fictional, the baseline accuracy is 0%.

This benchmark measures how well a Pupil agent retains and applies knowledge from structured curriculum that it has never seen during pre-training. The agent learned the material through Pupil's agentic learning process (reading, comprehending, and storing memories with entities, topics, and relationship links), not through fine-tuning, prompt injection, or retrieval-augmented generation with raw text chunks.

**Paper:** "PhantomWiki: On-Demand Datasets for Reasoning and Retrieval Evaluation" by Gong et al.
- arXiv: <https://arxiv.org/abs/2502.20377>
- GitHub: <https://github.com/kilian-group/phantom-wiki>
- HuggingFace dataset: [kilian-group/phantom-wiki-v1](https://huggingface.co/datasets/kilian-group/phantom-wiki-v1)

## Setup

**Agent configuration**: `examples/phantomwiki-agent/pupil.yaml`

| Parameter | Value |
|-----------|-------|
| Agent model | `vertex/gemini-2.5-flash` |
| Judge model | `vertex/gemini-2.5-flash` (tool-calling judge for structured scoring) |
| Curriculum | 51 character articles (one per fictional character) |
| Curriculum source | PhantomWiki `depth_20_size_50_seed_1` split |
| Memory backend | recalld v0.1.1 |
| Learning mode | Agentic learning with synthesis pass |

The curriculum covers 51 fictional character articles from the PhantomWiki dataset. During `pupil build`, the LLM reads each character article, comprehends the content, and stores structured memories in recalld. The learning process uses a multi-pass approach: an initial pass stores character facts, and a synthesis pass creates relationship links between characters.

## Methodology

The benchmark evaluation is separate from the learning process. The 112 PhantomWiki questions were never seen by the agent during learning.

**Learning phase** (during `pupil build`):
1. The agent reads all 51 curriculum files (one per character).
2. The LLM comprehends the content and stores memories with entities, topics, and tags.
3. A synthesis pass creates relationship links between characters in memory.
4. 1 source was skipped due to rate limiting (50 of 51 learned).
5. Only 3 tools are available during learning to reduce malformed tool calls.

**Evaluation phase** (after the build):
1. The 112 PhantomWiki questions are run against the built agent using `pupil test`.
2. Questions span depths 1 through 4, requiring increasingly complex reasoning chains.
3. The agent uses its recalld memory tools to recall relevant knowledge before answering.
4. Retrieval planning decomposes questions into multiple search queries.
5. Entity-filtered search narrows recall to relevant character names.
6. Graph traversal (depth 1-2) follows memory connections to find related facts.
7. Recalled memories are converted from JSON to plain text before being passed to the LLM.

**Judging**:
- Each answer is scored by an LLM judge (`vertex/gemini-2.5-flash`) using tool calling for structured output.
- Zero agent errors across all 112 questions.

## Results

**Overall: 63/112 passed (56.2%)**

| Depth | Questions | Passed | Accuracy |
|-------|-----------|--------|----------|
| Depth 1 (direct lookup) | 24 | 18 | 75% |
| Depth 2 (two-hop) | 42 | 24 | 57% |
| Depth 3 (three-hop) | 19 | 12 | 63% |
| Depth 4 (four-hop) | 27 | 9 | 33% |

Depth 1 questions are direct lookups (e.g. "Who is the wife of X?"). Depth 2 questions require two reasoning hops (e.g. "Who is the brother of the daughter of X?"). Depth 3 and 4 questions chain three and four hops respectively.

### Comparison with published results

The PhantomWiki paper (Table 2) reports F1 scores for several systems on the n=50 split, averaged over 3 seeds. The table below shows those results alongside the Pupil score.

**Pupil result (this evaluation):**

| System | Method | Metric | Score |
|--------|--------|--------|-------|
| Pupil (Gemini 2.5 Flash) | Agentic learning + memory recall | Pass rate (LLM judge) | 56.2% |

**Published results from PhantomWiki paper (Table 2):**

| System | Method | Metric | Score |
|--------|--------|--------|-------|
| DeepSeek-R1-32B | Chain-of-thought (full corpus in context) | F1 | 52.4% |
| GPT-4o | Chain-of-thought (full corpus in context) | F1 | 50.7% |
| GPT-4o | ReAct (agentic) | F1 | 38.7% |
| Llama-3.3-70B | ReAct (agentic) | F1 | 35.8% |
| Gemini 1.5 Flash | ReAct (agentic) | F1 | 30.9% |
| All models | Standard RAG (BM25) | F1 | ~20% |

**Important caveats on comparison:**

- **Different metrics.** The PhantomWiki paper uses F1 score. Pupil uses pass rate with an LLM judge. These are not directly comparable metrics. Pass rate counts a binary pass/fail per question; F1 measures token overlap between the predicted and gold answers. The numbers are presented together for context, not as an apples-to-apples comparison.
- **Different seeds.** The published scores are averaged over 3 seeds, while the Pupil result uses a single seed (seed 1 only).
- **Different access to the corpus.** The Chain-of-thought results give the model the full corpus in its context window. Pupil retrieves from memory, which may miss relevant facts. The CoT approach has a significant advantage on questions where all relevant information is present in context.

### Key features that contributed to the score

- **Agentic learning.** The LLM reads, comprehends, and stores structured memories with entities and topics, rather than chunking raw text.
- **Entity-filtered search.** recalld supports filtering recall results by entity names, allowing targeted retrieval for specific characters.
- **Graph traversal.** Depth 1-2 traversal follows memory connections to find related facts across characters.
- **Multi-pass learning.** A synthesis pass creates relationship links between characters after the initial learning pass.
- **Retrieval planning.** The agent decomposes multi-hop questions into multiple search queries before answering.
- **Humanized recall results.** JSON memory objects are converted to plain text before being passed to the LLM.
- **Tool filtering.** Only 3 tools are available during learning to reduce malformed tool calls.

## How to reproduce

Prerequisites: Pupil installed, a Google Cloud project with Vertex AI enabled, and Vertex AI credentials configured.

```bash
cd examples/phantomwiki-agent

# Download dataset, generate curriculum and benchmark files
./setup.sh

# Build the agent (runs the learning pipeline)
pupil build

# Run the PhantomWiki benchmark against the built agent
pupil test --file benchmark/phantomwiki.yaml
```

The `setup.sh` script downloads the PhantomWiki dataset and generates the curriculum files and benchmark test file. The build step takes time, as the agent reads and learns 51 character articles. The benchmark evaluation runs 112 sequential test queries against the agent.

## Limitations

- **Single seed.** The result uses seed 1 only, not averaged across multiple seeds as in the paper. Scores may differ on other seeds.
- **Metric difference.** Pupil uses pass rate with an LLM judge, while the paper uses F1 score. These metrics are related but not identical.
- **Domain-specific system prompt.** The `pupil.yaml` system prompt is tuned for the PhantomWiki domain (it mentions "fictional wiki" and "family relationships"). A generic system prompt may produce different results.
- **Depth 4 questions remain challenging.** The agent scored 33% on four-hop reasoning chains, indicating room for improvement on deeply compositional questions.
- **Rate limiting.** 1 of 51 sources was skipped due to rate limiting during the build, meaning the agent was missing one character's information.
- **Single model.** These results are for Gemini 2.5 Flash only. Different LLMs may produce different scores for both learning and answering.

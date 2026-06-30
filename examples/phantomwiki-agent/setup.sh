#!/usr/bin/env bash
set -euo pipefail

# Downloads the PhantomWiki dataset and generates curriculum files
# and benchmark questions for the phantomwiki-agent example.
#
# Requires: Python 3.8+, pip packages: datasets, pyyaml
#
# Usage:
#   cd examples/phantomwiki-agent
#   ./setup.sh

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

echo "Checking dependencies..."
python3 -c "import datasets, yaml" 2>/dev/null || {
    echo "Installing required packages..."
    pip3 install --quiet datasets pyyaml
}

echo "Downloading PhantomWiki dataset and generating files..."
python3 << 'PYEOF'
import os, yaml
from datasets import load_dataset

base = os.path.dirname(os.path.abspath("__file__"))
os.makedirs("curriculum", exist_ok=True)
os.makedirs("benchmark", exist_ok=True)

ds_corpus = load_dataset(
    "kilian-group/phantom-wiki-v1", "text-corpus",
    split="depth_20_size_50_seed_1"
)
ds_qa = load_dataset(
    "kilian-group/phantom-wiki-v1", "question-answer",
    split="depth_20_size_50_seed_1"
)

for doc in ds_corpus:
    slug = doc['title'].lower().replace(' ', '-')
    with open(f"curriculum/{slug}.md", 'w') as f:
        f.write(doc['article'])

tests = []
for q in ds_qa:
    if q['difficulty'] > 4:
        continue
    answers = q['answer']
    answer_str = ', '.join(sorted(answers))
    tests.append({
        'name': f"pw-{q['id']}",
        'question': q['question'],
        'expects': [{
            'llm_judge': {
                'criteria': (
                    f"The response must include the correct answer(s): {answer_str}. "
                    f"The response should name these people specifically. "
                    f"Partial matches count if at least one correct name is mentioned."
                ),
                'threshold': 0.7,
            }
        }]
    })

benchmark = {
    'config': {
        'temperature': 0,
        'retries': 0,
        'threshold': 0.7,
        'judge_model': 'vertex/gemini-2.5-flash',
    },
    'tests': tests,
}

with open("benchmark/phantomwiki.yaml", 'w') as f:
    yaml.dump(benchmark, f, default_flow_style=False, sort_keys=False,
              allow_unicode=True, width=200)

print(f"  {len(ds_corpus)} curriculum files written to curriculum/")
print(f"  {len(tests)} benchmark questions written to benchmark/phantomwiki.yaml")
PYEOF

echo "Done. Run 'pupil build' to teach the agent, then:"
echo "  pupil test --file benchmark/phantomwiki.yaml"

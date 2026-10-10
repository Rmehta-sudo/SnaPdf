# Project memory index

Search compact documents first and load only the topic needed for the task.

| Need | Read/search |
|---|---|
| Stable facts and provenance | `FACTS.md` |
| Goals, terminology, hypotheses, requirements | `topics/GOALS_AND_DOMAIN.md` |
| Runs, releases, evaluations, results | `topics/RUNS_AND_RESULTS.md` |
| Repositories, systems, environments, paths | `topics/SYSTEMS_AND_PATHS.md` |
| Decisions, incidents, errors, and fixes | `topics/DECISIONS_AND_INCIDENTS.md` |
| Artifacts and finalized procedures | `topics/ARTIFACTS_AND_PROCEDURES.md` |
| Exact historical statement | `evidence/thread_messages.jsonl` via targeted search |
| Coverage/source audit | `audit/chat_coverage.tsv`, `sources/rollouts.tsv` |

```bash
python3 scripts/project_memory/project_memory.py search 'QUERY'
python3 scripts/project_memory/project_memory.py search 'QUERY' --evidence
python3 scripts/project_memory/project_memory.py sync
python3 scripts/project_memory/project_memory.py validate
python3 scripts/project_memory/project_memory.py stats
```

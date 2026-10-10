# Memory maintenance contract

## Preserve

Record information a future agent could not reliably reconstruct if all chats vanished: user
instructions, domain definitions, hypotheses, decisions, exact identifiers/configurations/results,
paths, external identities, artifact locations, operational constraints, causal errors and fixes,
and finalized procedures. Every fact needs status and provenance.

## Do not canonicalize

Exclude greetings, repetition, status nudges, generic knowledge, secrets, raw logs, routine commands,
and implementation already reconstructable from versioned code. Retain uncertain material in cold
evidence until reviewed rather than discarding it.

## Source priority

Prefer: latest explicit user instruction; verified code/live evidence; canonical topics; root context;
dated reports; exact chat evidence. Mark conflicts superseded. Never infer current external state from
memory when it can be rechecked.

## Required cadence

- Assess durable changes on every substantive prompt and canonicalize them in that turn.
- Synchronize at the start of project work and at least every three substantive prompts.
- Synchronize after durable milestones and before handoff, compaction, archive, or completion.
- Update the root briefing only when the shared current picture changes.
- Validate before declaring completion. Do not pad canonical files when nothing durable changed.

The current assistant response may enter its rollout only after it is emitted; the next turn's first
sync captures it. Scripts maintain evidence and audits but do not replace semantic judgment.

# Runs and results

Record exact run/release/evaluation identities, configurations, lineage, metrics, conclusions, and
completion evidence. Do not compare configurations as controlled unless their differences are known.
# Runs and results

## 2026-10-10 scroll stress validation

- Input: `/home/rachitmehta/workspace/research-work/motion-project/paper-finds/datavista.pdf`
  (46 pages).
- Build: optimized local release after BUG-001 changes.
- Procedure: 220 rapid mouse-wheel steps down, followed by 220 rapid steps up.
- Result: process remained alive and responsive; observed RSS was approximately 195,528 KiB.
- Supporting checks: `cargo test --offline` passed both model tests; visual QA showed continuous page
  edges, small consistent inter-page spacing, vector sidebar icons, and no missing-glyph squares.

# Capsule plan v1

A capsule is an immutable, generation-bound bitmap of pages with mandatory and frontier subsets, a stable content-derived ID, exact byte count, profile dimensions, bounded risk evidence, and cluster provenance. Mandatory and frontier pages must be contained in the total set. Every provenance bitmap must also be contained in the plan.

Risk components are integer millionths: held-out violations, unseen-branch mass, data-quality loss, and version-transfer confidence. They are evidence fields, not claims of certainty.

Budgeted baselines (`mirage_simulator::budgeted`) evaluate the same held-out folds at a fixed page budget using simple deterministic policies — access order, frequency, recency, and co-access clustering — plus honest abstention when training evidence is below the configured minimum. A fold's deadline model is a synthetic ordering assumption (predicted pages arrive in ranked order; a page is late when its rank exceeds `lead_pages + first-touch index`), not a network model. These are baselines to beat at equal disk budget, not predictions of a real title's access stream.

Statistical discipline (`mirage_simulator::statistics`) implements the proposal's evidence rules: `zero_failure_upper_bound_millionths` gives the one-sided Clopper-Pearson upper bound after `n` zero-failure sessions, `sessions_required_for_zero_failure_bound` gives the session count a zero-failure claim needs, and the percentile helpers keep per-run medians and pooled percentiles distinct so a fat-tailed run cannot hide inside a pool. The pinned reference values are 300 zero-failure sessions ⇒ a ≈0.9936% upper bound at α = 5%, and a 0.01% claim ⇒ at least 29,956 sessions.

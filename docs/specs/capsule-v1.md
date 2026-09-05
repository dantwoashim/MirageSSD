# Capsule plan v1

A capsule is an immutable, generation-bound bitmap of pages with mandatory and frontier subsets, a stable content-derived ID, exact byte count, profile dimensions, bounded risk evidence, and cluster provenance. Mandatory and frontier pages must be contained in the total set. Every provenance bitmap must also be contained in the plan.

Risk components are integer millionths: held-out violations, unseen-branch mass, data-quality loss, and version-transfer confidence. They are evidence fields, not claims of certainty.

use crate::{ObjectiveWeights, SweepResult};
use mirage_types::MirageError;

pub const GATE_A_SYNTHETIC_WARNING: &str =
    "Synthetic traces are engineering fixtures, not evidence of real-game performance.";

pub fn render_markdown(
    results: &[SweepResult],
    weights: ObjectiveWeights,
) -> Result<String, MirageError> {
    if results.is_empty() {
        return Err(MirageError::invalid_argument(
            "cannot report an empty sweep",
        ));
    }
    let mut output = format!(
        "# Gate A simulator sweep\n\n> {GATE_A_SYNTHETIC_WARNING}\n\nObjective weights: blocking_ns={}, remote_bytes={}, cache_bytes={}.\n\n| case | page bytes | cache pages | blocking ns | remote bytes | objective | Pareto |\n|---:|---:|---:|---:|---:|---:|:---:|\n",
        weights.blocking_ns, weights.remote_bytes, weights.cache_bytes
    );
    for result in results {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            result.case.ordinal,
            result.case.page_bytes,
            result.case.cache_pages,
            result.metrics.blocking_ns,
            result.metrics.remote_bytes,
            result.objective,
            if result.pareto_optimal { "yes" } else { "no" }
        ));
    }
    Ok(output)
}

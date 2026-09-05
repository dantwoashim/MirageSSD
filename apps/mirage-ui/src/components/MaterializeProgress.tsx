export function MaterializeProgress({ completed, total }: { completed: number; total: number }) {
  const safeTotal = Math.max(1, total);
  const percent = Math.min(100, Math.round((completed / safeTotal) * 100));
  return (
    <div className="space-y-2" aria-live="polite">
      <div className="flex justify-between text-xs text-zinc-400">
        <span>Verifying local capsule</span>
        <span className="number">{percent}%</span>
      </div>
      <progress className="h-1.5 w-full overflow-hidden rounded-full accent-emerald-500" value={completed} max={safeTotal} aria-label="Capsule materialization" />
    </div>
  );
}

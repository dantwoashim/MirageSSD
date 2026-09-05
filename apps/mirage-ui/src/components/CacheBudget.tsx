import { HardDrive } from '@phosphor-icons/react';
import { formatBytes } from './CapsuleBreakdown';

export function validateBudget(bytes: number, mandatory: number, free: number): string | undefined {
  if (!Number.isSafeInteger(bytes) || bytes < mandatory) return 'Below the mandatory local minimum.';
  if (bytes > free) return 'Exceeds available NTFS space.';
  return undefined;
}

export function CacheBudget({
  bytes,
  mandatory,
  free,
  onChange,
}: {
  bytes: number;
  mandatory: number;
  free: number;
  onChange?: (bytes: number) => void;
}) {
  const error = validateBudget(bytes, mandatory, free);
  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-5">
        <div>
          <label htmlFor="cache-budget" className="flex items-center gap-2 text-sm font-medium text-zinc-100">
            <HardDrive size={17} weight="bold" aria-hidden="true" />
            Physical cache budget
          </label>
          <p className="mt-1 text-xs leading-relaxed text-zinc-500">Allocated NTFS bytes, including the update reserve.</p>
        </div>
        <output htmlFor="cache-budget" className="number text-sm text-zinc-200">{formatBytes(bytes)}</output>
      </div>
      <input
        id="cache-budget"
        className="h-1.5 w-full accent-emerald-500"
        type="range"
        min={mandatory}
        max={Math.max(mandatory, free)}
        step={Math.max(1, 1024 ** 3)}
        value={Math.min(Math.max(bytes, mandatory), Math.max(mandatory, free))}
        onChange={(event) => onChange?.(Number(event.target.value))}
        disabled={!onChange || free < mandatory}
      />
      <div className="flex justify-between text-xs text-zinc-600">
        <span>Minimum {formatBytes(mandatory)}</span>
        <span>Free {formatBytes(free)}</span>
      </div>
      <p aria-live="polite" className={`min-h-5 text-xs ${error ? 'text-amber-200' : 'text-zinc-500'}`}>
        {error ?? 'Budget fits the reported free-space boundary.'}
      </p>
    </div>
  );
}

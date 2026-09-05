import { ShieldWarning } from '@phosphor-icons/react';
import type { Readiness } from '../models';

export function RiskPanel({ value }: { value: Readiness }) {
  const clean = value.heldOutViolations === 0 && !value.lastSealViolation;
  return (
    <section aria-labelledby="risk-heading" className="border-l border-amber-300/30 pl-4">
      <div className="flex items-center gap-2">
        <ShieldWarning size={18} weight="bold" className={clean ? 'text-zinc-500' : 'text-amber-200'} aria-hidden="true" />
        <h3 id="risk-heading" className="text-sm font-semibold text-zinc-100">Measured risk</h3>
      </div>
      <p className="mt-2 text-sm leading-relaxed text-zinc-400">
        {clean
          ? 'No held-out violation is recorded for this capsule. That is evidence for this profile, not a universal score.'
          : `${value.heldOutViolations} held-out violation${value.heldOutViolations === 1 ? '' : 's'} remain in the selected profile.`}
      </p>
      {value.lastSealViolation && <p className="number mt-2 text-xs text-amber-100">Last: {value.lastSealViolation}</p>}
    </section>
  );
}

import { ShieldWarning } from '@phosphor-icons/react';
import type { Readiness } from '../models';
import { cx } from '../ui';

export function RiskPanel({ value }: { value: Readiness }) {
  const clean = value.heldOutViolations === 0 && !value.lastSealViolation;
  return (
    <section aria-labelledby="risk-heading" className="border-l-2 border-warn/40 pl-4">
      <div className="flex items-center gap-2">
        <ShieldWarning size={17} weight="bold" className={clean ? 'text-fg-subtle' : 'text-warn'} aria-hidden="true" />
        <h3 id="risk-heading" className="text-[13px] font-semibold text-fg">Measured risk</h3>
      </div>
      <p className="mt-2 text-[13px] leading-relaxed text-fg-muted">
        {clean
          ? 'No verification miss is recorded for this copy. That is evidence for this profile, not a universal score.'
          : `${value.heldOutViolations} held-out violation${value.heldOutViolations === 1 ? '' : 's'} remain in the selected profile.`}
      </p>
      {value.lastSealViolation && <p className={cx('number mt-2 text-xs text-warn')}>Last: {value.lastSealViolation}</p>}
    </section>
  );
}

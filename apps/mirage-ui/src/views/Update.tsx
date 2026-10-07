import { ArrowCounterClockwise, ArrowsClockwise, CaretRight, CheckCircle } from '@phosphor-icons/react';
import { Card } from '../ui';

export function UpdateView({ busy, onBegin, onRefresh, onCommit, onRollback }: { busy: boolean; onBegin: () => void; onRefresh: () => void; onCommit: () => void; onRollback: () => void }) {
  const actions = [
    { label: 'Begin exclusive update', detail: 'Unmounts play state and opens a durable journal.', Icon: ArrowsClockwise, action: onBegin },
    { label: 'Refresh journal state', detail: 'Reads the service-owned recovery status.', Icon: CheckCircle, action: onRefresh },
    { label: 'Activate the verified version', detail: 'Only succeeds after immutable upload verification.', Icon: CheckCircle, action: onCommit },
    { label: 'Roll back safely', detail: 'Restores exactly the previous version.', Icon: ArrowCounterClockwise, action: onRollback },
  ];
  return (
    <Card padding="lg" aria-labelledby="update-title">
      <h2 id="update-title" className="section-title">Update and recovery journal</h2>
      <p className="mt-2 max-w-[65ch] text-[13px] leading-relaxed text-fg-muted">MirageSSD never presents a half-applied version. Activation is an atomic switch after staged files verify.</p>
      <div className="mt-5 divide-y divide-line border-y border-line">
        {actions.map(({ label, detail, Icon, action }) => (
          <button key={label} type="button" onClick={action} disabled={busy} className="grid w-full grid-cols-[2rem_minmax(0,1fr)_auto] items-center gap-4 py-3.5 text-left transition-colors duration-150 ease-standard hover:bg-surface-2 disabled:opacity-50">
            <Icon size={18} weight="duotone" className="text-fg-muted" aria-hidden="true" />
            <span className="min-w-0">
              <span className="block text-[13px] font-medium text-fg">{label}</span>
              <span className="mt-0.5 block text-xs text-fg-subtle">{detail}</span>
            </span>
            <CaretRight size={15} className="text-fg-subtle" aria-hidden="true" />
          </button>
        ))}
      </div>
    </Card>
  );
}

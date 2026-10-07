import { Check, FileMagnifyingGlass, LockKey, ShieldCheck } from '@phosphor-icons/react';
import { Card } from '../ui';

const phases = [
  'Read-only source scan',
  'Classification evidence',
  'Upload and byte verification',
  'Local-provider differential test',
  'Native rollback point',
  'Explicit conversion confirmation',
  'Mounted full-local smoke test',
  'Separate reclaim confirmation',
];

export function ImportWizard() {
  return (
    <section className="grid gap-4 lg:grid-cols-[minmax(0,.78fr)_minmax(24rem,1.22fr)]" aria-labelledby="import-title">
      <Card padding="lg">
        <FileMagnifyingGlass size={24} weight="duotone" className="text-accent" aria-hidden="true" />
        <h2 id="import-title" className="section-title mt-4">Conversion starts read-only</h2>
        <p className="mt-2 text-[13px] leading-relaxed text-fg-muted">The desktop preview exposes the safety contract. Source discovery remains a CLI operation until a drive is configured.</p>
        <div className="mt-5 rounded-xl border border-line bg-surface-2 p-4">
          <p className="text-xs font-medium text-fg-muted">Read-only scan command</p>
          <code className="number mt-2 block overflow-x-auto text-xs text-accent">mirage repo scan &lt;game-root&gt; --report scan.json</code>
        </div>
        <div className="mt-5 flex gap-3 border-l-2 border-warn/40 pl-4 text-xs leading-relaxed text-fg-muted">
          <LockKey size={16} weight="bold" className="mt-0.5 shrink-0 text-warn" aria-hidden="true" />
          Original files cannot be reclaimed in the transaction that first creates a mount.
        </div>
      </Card>
      <Card padding="lg">
        <div className="flex items-center gap-2">
          <ShieldCheck size={18} weight="bold" className="text-fg-subtle" aria-hidden="true" />
          <h3 className="section-title text-[15px]">Mandatory conversion gates</h3>
        </div>
        <ol className="mt-5 divide-y divide-line border-y border-line">
          {phases.map((phase, index) => (
            <li key={phase} className="grid grid-cols-[2rem_minmax(0,1fr)_auto] items-center gap-3 py-3">
              <span className="number text-xs text-fg-subtle">{String(index + 1).padStart(2, '0')}</span>
              <span className="text-[13px] text-fg">{phase}</span>
              {index === 0 ? <Check size={15} weight="bold" className="text-ok" aria-label="Available" /> : <span className="text-[11px] text-fg-subtle">Locked</span>}
            </li>
          ))}
        </ol>
      </Card>
    </section>
  );
}

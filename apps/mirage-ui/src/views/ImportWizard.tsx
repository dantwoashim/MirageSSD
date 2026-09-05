import { Check, FileMagnifyingGlass, LockKey, ShieldCheck } from '@phosphor-icons/react';

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
    <section className="grid gap-6 lg:grid-cols-[minmax(0,.78fr)_minmax(24rem,1.22fr)]" aria-labelledby="import-title">
      <div className="surface rounded-[2rem] p-6 md:p-8">
        <FileMagnifyingGlass size={26} weight="duotone" className="text-emerald-200" aria-hidden="true" />
        <h2 id="import-title" className="mt-6 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Conversion starts read-only</h2>
        <p className="mt-3 text-sm leading-7 text-zinc-400">The desktop preview exposes the safety contract. Source discovery remains a CLI operation until a repository is configured.</p>
        <div className="mt-6 rounded-2xl border border-white/8 bg-[#111513] p-4">
          <p className="text-xs font-medium text-zinc-400">Read-only scan command</p>
          <code className="number mt-2 block overflow-x-auto text-xs text-emerald-200">mirage repo scan &lt;game-root&gt; --report scan.json</code>
        </div>
        <div className="mt-6 flex gap-3 border-l border-amber-300/30 pl-4 text-xs leading-6 text-zinc-500">
          <LockKey size={18} weight="bold" className="mt-0.5 shrink-0 text-amber-200" aria-hidden="true" />
          Original files cannot be reclaimed in the transaction that first creates a mount.
        </div>
      </div>
      <div className="rounded-[2rem] border border-white/8 p-6 md:p-8">
        <div className="flex items-center gap-2">
          <ShieldCheck size={19} weight="bold" className="text-zinc-500" aria-hidden="true" />
          <h3 className="text-sm font-semibold text-zinc-100">Mandatory conversion gates</h3>
        </div>
        <ol className="mt-6 divide-y divide-white/7 border-y border-white/8">
          {phases.map((phase, index) => (
            <li key={phase} className="grid grid-cols-[2rem_1fr_auto] items-center gap-3 py-4">
              <span className="number text-xs text-zinc-600">{String(index + 1).padStart(2, '0')}</span>
              <span className="text-sm text-zinc-300">{phase}</span>
              {index === 0 ? <Check size={16} weight="bold" className="text-emerald-300" aria-label="Available" /> : <span className="text-[11px] uppercase tracking-[0.12em] text-zinc-700">Locked</span>}
            </li>
          ))}
        </ol>
      </div>
    </section>
  );
}

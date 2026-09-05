import { Pulse, Wrench } from '@phosphor-icons/react';

export function Recovery({ busy, onRepair }: { busy: boolean; onRepair: () => void }) {
  return (
    <section className="grid gap-6 lg:grid-cols-[minmax(0,1.35fr)_minmax(18rem,.65fr)]" aria-labelledby="recovery-title">
      <div className="surface rounded-[2rem] p-6 md:p-8">
        <Pulse size={25} weight="duotone" className="text-emerald-200" aria-hidden="true" />
        <h2 id="recovery-title" className="mt-6 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Recover metadata before bytes</h2>
        <p className="mt-3 max-w-[65ch] text-sm leading-7 text-zinc-400">Repair is conservative: it checks local metadata and preserves dirty or recovery-pinned pages. It does not overwrite original game data.</p>
        <button onClick={onRepair} disabled={busy} className="mt-7 inline-flex items-center gap-2 rounded-xl bg-emerald-500 px-4 py-2.5 text-sm font-semibold text-[#101713] transition duration-300 ease-out hover:bg-emerald-400 active:translate-y-px disabled:bg-zinc-700 disabled:text-zinc-400">
          <Wrench size={17} weight="bold" aria-hidden="true" />
          {busy ? 'Checking' : 'Run conservative repair'}
        </button>
      </div>
      <aside className="rounded-[2rem] border border-white/8 p-6">
        <h3 className="text-sm font-semibold text-zinc-100">Recovery order</h3>
        <ol className="number mt-5 space-y-4 text-xs leading-6 text-zinc-500">
          <li>01  Stop launch activity</li>
          <li>02  Inspect journal state</li>
          <li>03  Resume or roll back</li>
          <li>04  Verify active generation</li>
          <li>05  Remount explicitly</li>
        </ol>
      </aside>
    </section>
  );
}

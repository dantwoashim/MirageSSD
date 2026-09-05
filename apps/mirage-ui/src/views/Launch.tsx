import { Play, ShieldCheck } from '@phosphor-icons/react';
import { motion } from 'framer-motion';
import { CapsuleBreakdown } from '../components/CapsuleBreakdown';
import { RiskPanel } from '../components/RiskPanel';
import type { Mode, Readiness } from '../models';

export const modeMeaning: Record<Mode, string> = {
  verified_local: 'The complete admitted byte set is local and origin access is forbidden after admission.',
};

export function canLaunch(mode: Mode, readiness: Readiness) {
  return mode === 'verified_local' && readiness.state === 'sealed_ready' && Boolean(readiness.capsuleId);
}

export function Launch({
  mode,
  readiness,
  busy,
  onMode,
  onPlan,
  onMaterialize,
  onAdmit,
  onLaunch,
}: {
  mode: Mode;
  readiness: Readiness;
  busy?: boolean;
  onMode: (mode: Mode) => void;
  onPlan: () => void;
  onMaterialize: () => void;
  onAdmit: () => void;
  onLaunch: () => void;
}) {
  const modes: Mode[] = ['verified_local'];
  return (
    <section className="grid gap-6 lg:grid-cols-[minmax(0,1.35fr)_minmax(19rem,.65fr)]" aria-labelledby="launch-title">
      <div className="surface rounded-[2rem] p-6 md:p-8">
        <div className="flex items-start justify-between gap-5">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.18em] text-emerald-300/70">Admission</p>
            <h2 id="launch-title" className="mt-2 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Choose the guarantee first</h2>
          </div>
          <span className={`status-breathe mt-1 size-2.5 rounded-full ${readiness.state === 'sealed_ready' ? 'bg-emerald-400' : 'bg-amber-300'}`} aria-hidden="true" />
        </div>
        <div className="relative mt-7 grid gap-2 sm:grid-cols-2">
          {modes.map((item) => (
            <button key={item} onClick={() => onMode(item)} className={`relative overflow-hidden rounded-2xl border px-4 py-4 text-left transition duration-300 ease-out active:translate-y-px ${mode === item ? 'border-emerald-300/30 bg-emerald-300/[0.07]' : 'border-white/8 bg-white/[0.02] hover:border-white/15'}`}>
              {mode === item && <motion.span layoutId="active-mode" className="absolute inset-y-3 left-0 w-0.5 rounded-full bg-emerald-300" transition={{ type: 'spring', stiffness: 100, damping: 20 }} />}
              <span className="block text-sm font-semibold capitalize text-zinc-100">{item.replace('_', ' ')}</span>
              <span className="mt-1.5 block text-xs leading-relaxed text-zinc-500">{modeMeaning[item]}</span>
            </button>
          ))}
        </div>
        <div className="mt-8">
          <CapsuleBreakdown value={readiness} />
        </div>
        <div className="mt-7 flex flex-col gap-3 sm:flex-row">
          <button onClick={onPlan} disabled={busy} className="rounded-xl border border-white/12 bg-white/5 px-4 py-2.5 text-sm font-medium text-zinc-200 transition duration-300 ease-out hover:bg-white/8 active:translate-y-px disabled:opacity-45">
            Plan capsule
          </button>
          <button onClick={onMaterialize} disabled={busy || !readiness.capsuleId || readiness.state === 'sealed_ready'} className="rounded-xl border border-white/12 bg-white/5 px-4 py-2.5 text-sm font-medium text-zinc-200 transition duration-300 ease-out hover:bg-white/8 active:translate-y-px disabled:opacity-45">
            Materialize
          </button>
          <button onClick={onAdmit} disabled={busy || !readiness.capsuleId || readiness.state !== 'materializing' || readiness.missingBytes !== 0} className="rounded-xl border border-white/12 bg-white/5 px-4 py-2.5 text-sm font-medium text-zinc-200 transition duration-300 ease-out hover:bg-white/8 active:translate-y-px disabled:opacity-45">
            Admit offline
          </button>
          <button disabled={busy || !canLaunch(mode, readiness)} onClick={onLaunch} className="inline-flex items-center justify-center gap-2 rounded-xl bg-emerald-500 px-4 py-2.5 text-sm font-semibold text-[#101713] transition duration-300 ease-out hover:bg-emerald-400 active:translate-y-px disabled:bg-zinc-700 disabled:text-zinc-400">
            {mode === 'verified_local' ? <ShieldCheck size={17} weight="bold" aria-hidden="true" /> : <Play size={17} weight="fill" aria-hidden="true" />}
            {busy ? 'Working' : 'Launch'}
          </button>
        </div>
      </div>
      <aside className="space-y-6 rounded-[2rem] border border-white/8 bg-white/[0.018] p-6 md:p-7">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.16em] text-zinc-600">Readiness state</p>
          <p className="number mt-2 text-lg text-zinc-100">{readiness.state.toUpperCase()}</p>
        </div>
        <RiskPanel value={readiness} />
        <p className="text-xs leading-6 text-zinc-600">The service is the final authority. UI checks can prevent an invalid request, but cannot approve a capsule.</p>
      </aside>
    </section>
  );
}

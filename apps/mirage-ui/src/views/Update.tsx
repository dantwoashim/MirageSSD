import { ThemeToggle } from '../components/ThemeToggle';
import { ArrowCounterClockwise, ArrowsClockwise, CheckCircle, DownloadSimple } from '@phosphor-icons/react';
import { useEffect, useState } from 'react';
import type { ServiceClient } from '../api/client';
import type { UpdateCheck } from '../models';

export function UpdateView({ client, busy, onBegin, onRefresh, onCommit, onRollback }: { client: ServiceClient; busy: boolean; onBegin: () => void; onRefresh: () => void; onCommit: () => void; onRollback: () => void }) {
  const [check, setCheck] = useState<UpdateCheck>();
  useEffect(() => {
    void client.updateCheck().then(setCheck).catch(() => {});
  }, [client]);
  const lastChecked = check?.checked_at ? new Date(check.checked_at * 1000).toLocaleString() : 'Not checked';
  const actions = [
    { label: 'Begin exclusive update', detail: 'Unmounts play state and opens a durable journal.', Icon: ArrowsClockwise, action: onBegin },
    { label: 'Refresh journal state', detail: 'Reads the service-owned recovery status.', Icon: CheckCircle, action: onRefresh },
    { label: 'Activate the verified version', detail: 'Only succeeds after immutable upload verification.', Icon: CheckCircle, action: onCommit },
    { label: 'Roll back safely', detail: 'Restores exactly the previous version.', Icon: ArrowCounterClockwise, action: onRollback },
  ];
  return (
    <section className="surface rounded-[2rem] p-6 md:p-8" aria-labelledby="update-title">
      <p className="text-xs font-semibold uppercase tracking-[0.18em] text-emerald-300/70">Exclusive writer</p>
      <h2 id="update-title" className="mt-2 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Update and recovery journal</h2>
      <p className="mt-3 max-w-[65ch] text-sm leading-7 text-zinc-400">MirageSSD never presents a half-applied version. Activation is an atomic switch after staged files verify.</p>
      {check?.checked && (
        <div className="app-update-card" role="status">
          <p className="text-xs text-zinc-500">
            MirageSSD {check.current} · {check.channel ?? 'preview'} channel · last checked {lastChecked}
          </p>
          {check.update_available && check.latest && (
            <a className="update-download" href={check.url} target="_blank" rel="noreferrer">
              <DownloadSimple size={14} />{check.latest.replace(/^v/, '')} is available — Download
            </a>
          )}
        </div>
      )}
      <ThemeToggle labelled />
      <div className="mt-8 divide-y divide-white/7 border-y border-white/8">
        {actions.map(({ label, detail, Icon, action }) => (
          <button key={label} onClick={action} disabled={busy} className="grid w-full grid-cols-[2.5rem_1fr_auto] items-center gap-4 py-4 text-left transition duration-300 ease-out hover:bg-white/[0.025] active:translate-y-px disabled:opacity-45">
            <Icon size={19} weight="duotone" className="text-zinc-500" aria-hidden="true" />
            <span><span className="block text-sm font-medium text-zinc-200">{label}</span><span className="mt-1 block text-xs text-zinc-600">{detail}</span></span>
            <span className="text-xs text-zinc-600">Run</span>
          </button>
        ))}
      </div>
    </section>
  );
}

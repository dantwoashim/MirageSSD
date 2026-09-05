import { LockKeyOpen, ShieldCheck } from '@phosphor-icons/react';

export function UninstallPreparation({ mounted }: { mounted: boolean }) {
  return (
    <section className="surface rounded-[2rem] p-6 md:p-8" aria-labelledby="uninstall-title">
      <div className="grid gap-8 md:grid-cols-[minmax(0,1fr)_minmax(17rem,.55fr)]">
        <div>
          <LockKeyOpen size={26} weight="duotone" className="text-emerald-200" aria-hidden="true" />
          <h2 id="uninstall-title" className="mt-6 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Prepare before uninstall</h2>
          <p className="mt-3 max-w-[60ch] text-sm leading-7 text-zinc-400">Restore-native reconstructs every virtual file in a new directory, verifies bytes, unmounts, and swaps the ordinary NTFS directory into place. Remote repositories remain user-owned.</p>
        </div>
        <div className="rounded-2xl border border-white/8 bg-[#111513] p-5">
          <div className="flex items-center gap-2">
            <ShieldCheck size={18} weight="bold" className={mounted ? 'text-amber-200' : 'text-emerald-200'} aria-hidden="true" />
            <p className="text-sm font-semibold text-zinc-100">Uninstall guard</p>
          </div>
          <p className="mt-3 text-sm leading-6 text-zinc-500">{mounted ? 'Blocked while this repository is mounted.' : 'No active mount is reported. Sessions and journals are checked again by the MSI.'}</p>
        </div>
      </div>
    </section>
  );
}

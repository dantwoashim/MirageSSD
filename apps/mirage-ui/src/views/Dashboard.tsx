import { ArrowClockwise, CaretRight, Database, HardDrives } from '@phosphor-icons/react';
import { motion } from 'framer-motion';
import type { RepositoryState } from '../models';
import { formatBytes } from '../components/CapsuleBreakdown';
import { HealthBadge } from '../components/HealthBadge';

export function Dashboard({
  repositories,
  selectedId,
  onSelect,
  onRefresh,
  busy = false,
  onSetup,
}: {
  repositories: RepositoryState[];
  selectedId?: string;
  onSelect: (repositoryId: string) => void;
  onRefresh: () => void;
  busy?: boolean;
  onSetup?: () => void;
}) {
  if (repositories.length === 0) {
    return (
      <section className="surface fine-grid min-h-96 rounded-[2rem] p-7 md:p-10" aria-labelledby="empty-title">
        <div className="max-w-xl pt-16 md:ml-[12%]">
          <span className="grid size-12 place-items-center rounded-2xl border border-white/10 bg-white/4 text-emerald-200">
            <HardDrives size={24} weight="duotone" aria-hidden="true" />
          </span>
          <h2 id="empty-title" className="mt-6 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">A fresh start for your files.</h2>
          <p className="mt-3 max-w-[58ch] text-sm leading-7 text-zinc-400">
            Add a workspace to see its local storage, prepare offline access, and manage recovery in one place.
          </p>
          <button className="primary-button mt-6" onClick={onSetup}>Set up a workspace <CaretRight size={16} /></button>
        </div>
      </section>
    );
  }

  return (
    <section aria-labelledby="repositories-title">
      <div className="mb-5 flex items-end justify-between gap-5">
        <div>
          <p className="eyebrow">Connected to your desktop</p>
          <h2 id="repositories-title" className="mt-2 text-xl font-semibold tracking-[-0.025em] text-zinc-50">Your workspaces <span className="count-label">{repositories.length}</span></h2>
        </div>
        <button onClick={onRefresh} disabled={busy} className="quiet-button">
          <ArrowClockwise size={16} weight="bold" aria-hidden="true" />
          Refresh
        </button>
      </div>
      <motion.div initial="hidden" animate="visible" variants={{ visible: { transition: { staggerChildren: 0.07 } } }} className="divide-y divide-white/7 border-y border-white/8">
        {repositories.map((repository) => (
          <motion.button
            variants={{ hidden: { opacity: 0, y: 8 }, visible: { opacity: 1, y: 0 } }}
            transition={{ type: 'spring', stiffness: 100, damping: 20 }}
            key={repository.id}
            onClick={() => onSelect(repository.id)}
            disabled={busy}
            aria-pressed={selectedId === repository.id}
            className={`grid w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-6 py-5 text-left transition duration-300 ease-out hover:bg-white/[0.025] active:translate-y-px md:grid-cols-[minmax(0,1.4fr)_minmax(8rem,.6fr)_minmax(8rem,.6fr)_auto] ${selectedId === repository.id ? 'bg-white/[0.035]' : ''}`}
          >
            <div className="flex min-w-0 items-center gap-4 px-2">
              <span className="grid size-10 shrink-0 place-items-center rounded-xl border border-white/8 bg-zinc-900/70 text-zinc-400">
                <Database size={19} weight="duotone" aria-hidden="true" />
              </span>
              <div className="min-w-0">
                <h3 className="truncate text-sm font-semibold text-zinc-100">{repository.name}</h3>
                <p className="mt-1 truncate text-xs text-zinc-500">{repository.mounted ? 'Connected to your file manager' : 'Available on this desktop'}</p>
              </div>
            </div>
            <div className="hidden md:block">
              <p className="text-xs text-zinc-500">Version</p>
              <p className="number mt-1 text-sm text-zinc-300">{repository.generation ?? 'None'}</p>
            </div>
            <div className="hidden md:block">
              <p className="text-xs text-zinc-500">On this device</p>
              <p className="number mt-1 text-sm text-zinc-300">{formatBytes(repository.physicalBytes)}</p>
            </div>
            <div className="flex items-center gap-3 pr-2">
              <HealthBadge value={repository.state} />
              <CaretRight size={16} className="text-zinc-600" aria-hidden="true" />
            </div>
          </motion.button>
        ))}
      </motion.div>
    </section>
  );
}

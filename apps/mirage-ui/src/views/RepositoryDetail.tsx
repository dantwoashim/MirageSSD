import { Cloud, HardDrive, ShieldCheck } from '@phosphor-icons/react';
import { formatBytes } from '../components/CapsuleBreakdown';
import { HealthBadge } from '../components/HealthBadge';
import type { RepositoryState } from '../models';

export function RepositoryDetail({ repository }: { repository: RepositoryState }) {
  const metrics = [
    { label: 'Logical assets', value: formatBytes(repository.logicalBytes), Icon: HardDrive },
    { label: 'Physical cache', value: formatBytes(repository.physicalBytes), Icon: ShieldCheck },
    { label: 'Backend', value: repository.backendHealth.replaceAll('_', ' '), Icon: Cloud },
  ];
  return (
    <section className="surface rounded-[2rem] p-6 md:p-8" aria-labelledby="repository-title">
      <div className="flex flex-col justify-between gap-5 border-b border-white/8 pb-6 md:flex-row md:items-start">
        <div>
          <p className="text-xs font-semibold uppercase tracking-[0.18em] text-emerald-300/70">Selected repository</p>
          <h2 id="repository-title" className="mt-2 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">{repository.name}</h2>
          <p className="number mt-2 text-xs text-zinc-600">{repository.id}</p>
        </div>
        <HealthBadge value={repository.state} />
      </div>
      <div className="grid gap-px bg-white/7 md:grid-cols-[1.25fr_.75fr_1fr]">
        {metrics.map(({ label, value, Icon }) => (
          <div key={label} className="bg-[#191f1c] px-1 py-6 md:px-6">
            <Icon size={18} weight="duotone" className="text-zinc-500" aria-hidden="true" />
            <p className="mt-5 text-xs uppercase tracking-[0.14em] text-zinc-600">{label}</p>
            <p className="number mt-1 text-base text-zinc-200 capitalize">{value}</p>
          </div>
        ))}
      </div>
      <div className="mt-6 grid gap-4 text-sm text-zinc-400 sm:grid-cols-2">
        <p>Active generation <span className="number ml-1 text-zinc-200">{repository.generation ?? 'not committed'}</span></p>
        <p>Last seal violations <span className="number ml-1 text-zinc-200">{repository.lastSealViolations}</span></p>
      </div>
    </section>
  );
}

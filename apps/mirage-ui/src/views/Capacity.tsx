import { CheckCircle, CloudSlash, HardDrive, LockKey, Trash } from '@phosphor-icons/react';
import { formatBytes } from '../components/CapsuleBreakdown';
import type { CapacityLease, CapacityPlan } from '../models';

export function CapacityView({
  requestedGiB,
  plan,
  lease,
  busy,
  onRequestedGiB,
  onPlan,
  onAcquire,
  onRelease,
}: {
  requestedGiB: number;
  plan?: CapacityPlan;
  lease?: CapacityLease;
  busy: boolean;
  onRequestedGiB: (value: number) => void;
  onPlan: () => void;
  onAcquire: () => void;
  onRelease: () => void;
}) {
  return (
    <section className="grid gap-6 lg:grid-cols-[minmax(0,1.3fr)_minmax(19rem,.7fr)]" aria-labelledby="capacity-title">
      <div className="surface rounded-[2rem] p-6 md:p-8">
        <p className="text-xs font-semibold uppercase tracking-[0.18em] text-emerald-300/70">Space Lease</p>
        <h2 id="capacity-title" className="mt-2 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">Reserve real local capacity</h2>
        <p className="mt-3 max-w-[68ch] text-sm leading-6 text-zinc-500">
          Mirage measures the target NTFS volume, protects its safety reserve, and only counts bytes it can actually free. Google Drive quota is never shown as local disk space.
        </p>

        <div className="mt-7 flex flex-col gap-3 sm:flex-row sm:items-end">
          <label className="block flex-1 text-xs font-semibold uppercase tracking-[0.13em] text-zinc-500">
            Capacity needed (GiB)
            <input
              type="number"
              min="1"
              max="1048576"
              step="1"
              value={requestedGiB}
              onChange={(event) => onRequestedGiB(Number(event.target.value))}
              className="number mt-2 w-full rounded-xl border border-white/12 bg-black/15 px-4 py-3 text-base text-zinc-100"
            />
          </label>
          <button onClick={onPlan} disabled={busy || !validGiB(requestedGiB)} className="rounded-xl border border-white/12 bg-white/5 px-5 py-3 text-sm font-medium text-zinc-200 transition hover:bg-white/8 disabled:opacity-45">
            {busy ? 'Measuring' : 'Analyze disk'}
          </button>
        </div>

        {plan && (
          <>
            <dl className="mt-8 grid gap-px overflow-hidden rounded-2xl border border-white/8 bg-white/8 sm:grid-cols-2">
              <Metric label="Physical free now" value={plan.physical_free_bytes} />
              <Metric label="Protected reserve" value={plan.filesystem_reserve_bytes} />
              <Metric label="Immediately usable" value={plan.immediately_available_bytes} />
              <Metric label="Safely reclaimable" value={plan.total_reclaimable_bytes} />
              <Metric label="Selected reclaim" value={plan.selected_reclaim_bytes} />
              <Metric label="Remaining shortfall" value={plan.shortfall_bytes} danger={plan.shortfall_bytes > 0} />
            </dl>
            <div className={`mt-5 rounded-2xl border p-4 text-sm ${plan.grantable ? 'border-emerald-300/20 bg-emerald-300/[0.055] text-emerald-100' : 'border-amber-300/20 bg-amber-300/[0.055] text-amber-100'}`}>
              <div className="flex items-start gap-3">
                {plan.grantable ? <CheckCircle className="mt-0.5 shrink-0" size={18} weight="fill" /> : <CloudSlash className="mt-0.5 shrink-0" size={18} weight="bold" />}
                <div>
                  <p className="font-semibold">{plan.grantable ? 'This lease can be prepared safely.' : `Free ${formatBytes(plan.shortfall_bytes)} more on the target volume.`}</p>
                  <p className="mt-1 text-xs leading-5 opacity-70">
                    {plan.selected_native_backup_count > 0
                      ? `A fully verified cloud-clean native tree is in the Eviction Bank and can surrender its SSD blocks.`
                      : plan.selected_drive_shadow_pack_count > 0
                      ? `${plan.selected_drive_shadow_pack_count} authenticated Drive-backed pack replica${plan.selected_drive_shadow_pack_count === 1 ? '' : 's'} can be removed.`
                      : plan.origin === 'drive' && !plan.drive_authenticated && plan.blocked.unverified_bytes > 0
                        ? `${formatBytes(plan.blocked.unverified_bytes)} remains blocked until Drive is authenticated through the secure CLI flow.`
                        : `${plan.selected_cache_page_count} verified cache page${plan.selected_cache_page_count === 1 ? '' : 's'} selected.`}
                  </p>
                </div>
              </div>
            </div>
            <div className="mt-5 flex flex-col gap-3 sm:flex-row">
              <button onClick={onAcquire} disabled={busy || !plan.grantable || Boolean(lease)} className="inline-flex items-center justify-center gap-2 rounded-xl bg-emerald-500 px-5 py-3 text-sm font-semibold text-[#101713] transition hover:bg-emerald-400 disabled:bg-zinc-700 disabled:text-zinc-400">
                <LockKey size={17} weight="bold" />Prepare six-hour lease
              </button>
            </div>
          </>
        )}
      </div>

      <aside className="space-y-6 rounded-[2rem] border border-white/8 bg-white/[0.018] p-6 md:p-7">
        <div className="flex items-center gap-3">
          <HardDrive size={19} weight="duotone" className="text-emerald-300" />
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.15em] text-zinc-600">Target volume</p>
            <p className="number mt-1 break-all text-sm text-zinc-200">{plan?.target_volume_id ?? 'Measure to identify'}</p>
          </div>
        </div>
        {lease ? (
          <div className="rounded-2xl border border-emerald-300/20 bg-emerald-300/[0.05] p-4">
            <p className="text-xs font-semibold uppercase tracking-[0.14em] text-emerald-200/70">Active lease</p>
            <p className="number mt-2 text-sm text-emerald-100">{formatBytes(lease.requested_bytes)}</p>
            <p className="number mt-2 break-all text-[10px] text-emerald-100/45">{lease.lease_id}</p>
            <button onClick={onRelease} disabled={busy} className="mt-4 inline-flex items-center gap-2 text-xs font-semibold text-zinc-300 disabled:opacity-45">
              <Trash size={14} weight="bold" />Release promise
            </button>
          </div>
        ) : <p className="text-xs leading-6 text-zinc-600">No local-space promise is active in this UI session.</p>}
        <p className="border-t border-white/8 pt-5 text-xs leading-6 text-zinc-600">A lease reserves physical headroom; it does not inflate Explorer capacity and it never treats remote quota as an SSD.</p>
      </aside>
    </section>
  );
}

function Metric({ label, value, danger = false }: { label: string; value: number; danger?: boolean }) {
  return (
    <div className="bg-[#191f1c] p-4">
      <dt className="text-xs text-zinc-600">{label}</dt>
      <dd className={`number mt-1 text-sm ${danger ? 'text-amber-200' : 'text-zinc-200'}`}>{formatBytes(value)}</dd>
    </div>
  );
}

function validGiB(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 1 && value <= 1_048_576;
}

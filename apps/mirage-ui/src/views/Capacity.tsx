import { HardDrive, LockKey, Trash } from '@phosphor-icons/react';
import { formatBytes } from '../format';
import type { CapacityLease, CapacityPlan } from '../models';
import { Button, Card, Input, Notice, cx } from '../ui';

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
    <section className="grid gap-4 lg:grid-cols-[minmax(0,1.3fr)_minmax(19rem,.7fr)]" aria-labelledby="capacity-title">
      <Card padding="lg">
        <h2 id="capacity-title" className="section-title">Reserve local capacity</h2>
        <p className="mt-2 max-w-[68ch] text-[13px] leading-relaxed text-fg-muted">
          Mirage measures the target NTFS volume, protects its safety reserve, and only counts bytes it can actually free. Google Drive quota is never shown as local disk space.
        </p>

        <div className="mt-6 flex flex-col gap-3 sm:flex-row sm:items-end">
          <label className="field-label block flex-1">
            Capacity needed (GiB)
            <Input
              type="number"
              min="1"
              max="1048576"
              step="1"
              value={requestedGiB}
              onChange={(event) => onRequestedGiB(Number(event.target.value))}
              className="number mt-1.5"
            />
          </label>
          <Button variant="secondary" onClick={onPlan} disabled={busy || !validGiB(requestedGiB)}>
            {busy ? 'Measuring' : 'Analyze disk'}
          </Button>
        </div>

        {plan && (
          <>
            <dl className="mt-6 grid gap-px overflow-hidden rounded-xl border border-line bg-line sm:grid-cols-2">
              <Metric label="Physical free now" value={plan.physical_free_bytes} />
              <Metric label="Protected reserve" value={plan.filesystem_reserve_bytes} />
              <Metric label="Immediately usable" value={plan.immediately_available_bytes} />
              <Metric label="Safely reclaimable" value={plan.total_reclaimable_bytes} />
              <Metric label="Selected reclaim" value={plan.selected_reclaim_bytes} />
              <Metric label="Remaining shortfall" value={plan.shortfall_bytes} danger={plan.shortfall_bytes > 0} />
            </dl>
            <Notice tone={plan.grantable ? 'success' : 'warning'} className="mt-4"
              title={plan.grantable ? 'This reservation can be prepared safely.' : `Free ${formatBytes(plan.shortfall_bytes)} more on the target volume.`}>
              <p>
                {plan.selected_native_backup_count > 0
                  ? 'A fully verified cloud-clean native tree is in the Eviction Bank and can surrender its SSD blocks.'
                  : plan.selected_drive_shadow_pack_count > 0
                  ? `${plan.selected_drive_shadow_pack_count} authenticated Drive-backed pack replica${plan.selected_drive_shadow_pack_count === 1 ? '' : 's'} can be removed.`
                  : plan.origin === 'drive' && !plan.drive_authenticated && plan.blocked.unverified_bytes > 0
                    ? `${formatBytes(plan.blocked.unverified_bytes)} remains blocked until Drive is authenticated through the secure CLI flow.`
                    : `${plan.selected_cache_page_count} verified cache page${plan.selected_cache_page_count === 1 ? '' : 's'} selected.`}
              </p>
            </Notice>
            <div className="mt-4 flex flex-col gap-3 sm:flex-row">
              <Button icon={<LockKey size={16} weight="bold" />} onClick={onAcquire} disabled={busy || !plan.grantable || Boolean(lease)}>
                Reserve space for six hours
              </Button>
            </div>
          </>
        )}
      </Card>

      <Card padding="lg" className="content-start">
        <div className="flex items-center gap-3">
          <HardDrive size={19} weight="duotone" className="text-accent" aria-hidden="true" />
          <div className="min-w-0">
            <p className="field-label">Target volume</p>
            <p className="number mt-1 break-all text-[13px] text-fg">{plan?.target_volume_id ?? 'Measure to identify'}</p>
          </div>
        </div>
        {lease ? (
          <div className="mt-5 rounded-xl border border-ok/25 bg-ok-soft p-4">
            <p className="field-label text-ok">Active reservation</p>
            <p className="number mt-2 text-[13px] text-fg">{formatBytes(lease.requested_bytes)}</p>
            <details className="mt-2 text-[11px] text-fg-muted"><summary className="cursor-pointer">Details</summary><span className="number break-all">{lease.lease_id}</span></details>
            <div className="mt-3">
              <Button variant="ghost" size="sm" icon={<Trash size={13} weight="bold" />} onClick={onRelease} disabled={busy}>
                Release promise
              </Button>
            </div>
          </div>
        ) : <p className="mt-5 text-xs leading-relaxed text-fg-subtle">No local-space promise is active in this UI session.</p>}
        <p className="mt-5 border-t border-line pt-4 text-xs leading-relaxed text-fg-subtle">A reservation holds physical headroom; it never inflates Explorer capacity or treats Drive quota as local space.</p>
      </Card>
    </section>
  );
}

function Metric({ label, value, danger = false }: { label: string; value: number; danger?: boolean }) {
  return (
    <div className="bg-surface p-4">
      <dt className="text-xs text-fg-subtle">{label}</dt>
      <dd className={cx('number mt-1 text-sm', danger ? 'text-warn' : 'text-fg')}>{formatBytes(value)}</dd>
    </div>
  );
}

function validGiB(value: number): boolean {
  return Number.isSafeInteger(value) && value >= 1 && value <= 1_048_576;
}

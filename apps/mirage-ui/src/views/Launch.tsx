import { ShieldCheck } from '@phosphor-icons/react';
import { CapsuleBreakdown } from '../components/CapsuleBreakdown';
import { RiskPanel } from '../components/RiskPanel';
import type { Mode, Readiness } from '../models';
import { canLaunch } from '../presentation';
import { Button, Card, cx } from '../ui';
export { canLaunch } from '../presentation';

export const modeMeaning: Record<Mode, string> = {
  verified_local: 'The selected files must be downloaded and verified before offline access is ready.',
};

export function Launch({
  mode,
  readiness,
  busy,
  onPlan,
  onMaterialize,
  onAdmit,
  onLaunch,
}: {
  mode: Mode;
  readiness: Readiness;
  busy?: boolean;
  onPlan: () => void;
  onMaterialize: () => void;
  onAdmit: () => void;
  onLaunch: () => void;
}) {
  return (
    <section className="grid gap-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(19rem,.65fr)]" aria-labelledby="launch-title">
      <Card padding="lg">
        <div className="flex items-start justify-between gap-5">
          <div>
            <h2 id="launch-title" className="section-title">Offline preparation</h2>
            <p className="mt-1.5 text-[13px] leading-relaxed text-fg-muted">
              One verified mode keeps the contract simple: everything needed locally is measured, downloaded, and checked.
            </p>
          </div>
          <span className={cx('mt-1.5 size-2.5 shrink-0 rounded-full', readiness.state === 'sealed_ready' ? 'bg-ok' : 'bg-warn')} aria-hidden="true" />
        </div>
        <div className="mt-5 rounded-xl border border-line bg-surface-2 p-4">
          <p className="text-[13px] font-semibold text-fg">Verified offline access</p>
          <p className="mt-1 text-xs leading-relaxed text-fg-muted">{modeMeaning[mode]}</p>
        </div>
        <div className="mt-6">
          <CapsuleBreakdown value={readiness} />
        </div>
        <div className="mt-6 flex flex-col gap-2.5 sm:flex-row">
          <Button variant="secondary" onClick={onPlan} disabled={busy}>
            1. Check files
          </Button>
          <Button variant="secondary" onClick={onMaterialize} disabled={busy || !readiness.capsuleId || readiness.state === 'sealed_ready'}>
            2. Download files
          </Button>
          <Button variant="secondary" onClick={onAdmit} disabled={busy || !readiness.capsuleId || readiness.state !== 'materializing' || readiness.missingBytes !== 0}>
            3. Verify offline access
          </Button>
          <Button icon={<ShieldCheck size={16} weight="bold" aria-hidden="true" />} disabled={busy || !canLaunch(mode, readiness)} onClick={onLaunch}>
            {busy ? 'Working' : 'Launch'}
          </Button>
        </div>
      </Card>
      <Card padding="lg" className="content-start">
        <div>
          <p className="field-label">Offline status</p>
          <p className="mt-2 text-[15px] font-medium text-fg">{readiness.state === 'sealed_ready' ? 'Ready to use offline' : readiness.state === 'materializing' ? 'Ready for verification' : 'Preparation needed'}</p>
        </div>
        <div className="mt-5">
          <RiskPanel value={readiness} />
        </div>
        <p className="mt-5 border-t border-line pt-4 text-xs leading-relaxed text-fg-subtle">Readiness applies to the verified version of this workspace. New versions need a fresh check.</p>
      </Card>
    </section>
  );
}

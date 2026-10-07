import { Pulse, Wrench } from '@phosphor-icons/react';
import { Button, Card } from '../ui';

export function Recovery({ busy, onRepair }: { busy: boolean; onRepair: () => void }) {
  return (
    <section className="grid gap-4 lg:grid-cols-[minmax(0,1.35fr)_minmax(18rem,.65fr)]" aria-labelledby="recovery-title">
      <Card padding="lg">
        <Pulse size={24} weight="duotone" className="text-accent" aria-hidden="true" />
        <h2 id="recovery-title" className="section-title mt-4">Recover metadata before bytes</h2>
        <p className="mt-2 max-w-[65ch] text-[13px] leading-relaxed text-fg-muted">Repair is conservative: it checks local metadata and preserves dirty or recovery-pinned pages. It does not overwrite original game data.</p>
        <div className="mt-6">
          <Button icon={<Wrench size={16} weight="bold" aria-hidden="true" />} onClick={onRepair} disabled={busy}>
            {busy ? 'Checking' : 'Run conservative repair'}
          </Button>
        </div>
      </Card>
      <Card padding="lg" className="content-start">
        <h3 className="section-title text-[15px]">Recovery order</h3>
        <ol className="number mt-4 grid gap-3.5 text-xs leading-relaxed text-fg-muted">
          <li>01  Stop launch activity</li>
          <li>02  Inspect journal state</li>
          <li>03  Resume or roll back</li>
          <li>04  Verify the current version</li>
          <li>05  Remount explicitly</li>
        </ol>
      </Card>
    </section>
  );
}

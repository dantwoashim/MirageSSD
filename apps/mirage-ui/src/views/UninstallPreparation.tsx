import { LockKeyOpen, ShieldCheck } from '@phosphor-icons/react';
import { Card } from '../ui';

export function UninstallPreparation({ mounted }: { mounted: boolean }) {
  return (
    <Card padding="lg" aria-labelledby="uninstall-title">
      <div className="grid gap-6 md:grid-cols-[minmax(0,1fr)_minmax(17rem,.55fr)]">
        <div>
          <LockKeyOpen size={24} weight="duotone" className="text-accent" aria-hidden="true" />
          <h2 id="uninstall-title" className="section-title mt-4">Prepare before uninstall</h2>
          <p className="mt-2 max-w-[60ch] text-[13px] leading-relaxed text-fg-muted">Restore-native reconstructs every virtual file in a new directory, verifies bytes, unmounts, and swaps the ordinary NTFS directory into place. Remote repositories remain user-owned.</p>
        </div>
        <div className="rounded-xl border border-line bg-surface-2 p-4">
          <div className="flex items-center gap-2">
            <ShieldCheck size={17} weight="bold" className={mounted ? 'text-warn' : 'text-ok'} aria-hidden="true" />
            <p className="text-[13px] font-semibold text-fg">Uninstall guard</p>
          </div>
          <p className="mt-2.5 text-[13px] leading-relaxed text-fg-muted">{mounted ? 'Blocked while this drive is connected.' : 'No active mount is reported. Sessions and journals are checked again by the MSI.'}</p>
        </div>
      </div>
    </Card>
  );
}

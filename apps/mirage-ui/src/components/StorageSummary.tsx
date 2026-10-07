import { CloudArrowUp, HardDrive } from '@phosphor-icons/react';
import { formatBytes } from '../format';
import type { RepositoryState } from '../models';
import { HealthBadge } from './HealthBadge';
import { Card, Notice } from '../ui';

export function StorageSummary({ repository }: { repository: RepositoryState }) {
  const pending = repository.pendingBytes ?? null;
  return (
    <Card padding="lg" aria-labelledby="repository-title">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="field-label">Selected workspace</p>
          <h2 id="repository-title" className="section-title mt-1.5 text-xl">{repository.name}</h2>
        </div>
        <HealthBadge value={repository.state} />
      </div>
      <dl className="mt-6 grid gap-4 sm:grid-cols-3">
        <div>
          <dt className="flex items-center gap-1.5 text-xs text-fg-muted"><HardDrive size={15} aria-hidden="true" />On this PC</dt>
          <dd className="number mt-1.5 text-2xl text-fg">{formatBytes(repository.physicalBytes)}</dd>
          <p className="mt-1 text-xs text-fg-subtle">Physical storage in use</p>
        </div>
        <div>
          <dt className="flex items-center gap-1.5 text-xs text-fg-muted"><CloudArrowUp size={15} aria-hidden="true" />Waiting to upload</dt>
          <dd className="number mt-1.5 text-2xl text-fg">{formatBytes(pending)}</dd>
          <p className="mt-1 text-xs text-fg-subtle">{pending === null ? 'Cloud status is not available yet' : pending === 0 ? 'No file-data uploads reported pending' : 'Keep MirageSSD connected to finish'}</p>
        </div>
        <div>
          <dt className="text-xs text-fg-muted">Total workspace</dt>
          <dd className="number mt-1.5 text-2xl text-fg">{formatBytes(repository.logicalBytes)}</dd>
          <p className="mt-1 text-xs text-fg-subtle">Logical size of your files</p>
        </div>
      </dl>
      {repository.diverged === true && (
        <Notice tone="warning" className="mt-5">
          <p>Different versions need review. Keep both copies until the conflict is resolved.</p>
        </Notice>
      )}
      <details className="mt-5 border-t border-line pt-4">
        <summary className="cursor-pointer text-xs font-medium text-fg-muted transition-colors duration-150 ease-standard hover:text-fg">Workspace details</summary>
        <dl className="mt-4 grid gap-4 sm:grid-cols-2">
          <div><dt className="text-xs text-fg-subtle">Version</dt><dd className="mt-1 text-[13px] text-fg">{repository.generation ?? 'Not available'}</dd></div>
          <div><dt className="text-xs text-fg-subtle">Storage connection</dt><dd className="mt-1 text-[13px] text-fg">{repository.backendHealth === 'unknown' ? 'Not checked' : repository.backendHealth.replaceAll('_', ' ')}</dd></div>
          <div><dt className="text-xs text-fg-subtle">Workspace ID</dt><dd className="number mt-1 break-all text-[13px] text-fg">{repository.id}</dd></div>
          {repository.volumeMode && <div><dt className="text-xs text-fg-subtle">Volume</dt><dd className="mt-1 text-[13px] text-fg">{repository.volumeMode === 'managed' ? 'Managed workspace' : 'Immutable workspace'}</dd></div>}
        </dl>
      </details>
    </Card>
  );
}

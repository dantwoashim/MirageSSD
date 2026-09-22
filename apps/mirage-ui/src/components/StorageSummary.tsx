import { CloudArrowUp, HardDrive, Info } from '@phosphor-icons/react';
import { formatBytes } from './CapsuleBreakdown';
import { HealthBadge } from './HealthBadge';
import type { RepositoryState } from '../models';

export function StorageSummary({ repository }: { repository: RepositoryState }) {
  const pending = repository.pendingBytes ?? null;
  return <section className="storage-summary" aria-labelledby="repository-title">
    <div className="storage-summary-heading"><div><span className="eyebrow">Selected workspace</span><h2 id="repository-title">{repository.name}</h2></div><HealthBadge value={repository.state} /></div>
    <dl className="storage-metrics">
      <div><dt><HardDrive size={18} />On this device</dt><dd>{formatBytes(repository.physicalBytes)}</dd><p>Physical storage in use</p></div>
      <div><dt><CloudArrowUp size={18} />Waiting to upload</dt><dd>{formatBytes(pending)}</dd><p>{pending === null ? 'Cloud status is not available yet' : pending === 0 ? 'No file-data uploads reported pending' : 'Keep MirageSSD connected to finish'}</p></div>
      <div><dt>Total workspace</dt><dd>{formatBytes(repository.logicalBytes)}</dd><p>Logical size of your files</p></div>
    </dl>
    {repository.diverged === true && <div className="feedback feedback-warning" role="alert"><Info size={20} /><p>Different versions need review. Keep both copies until the conflict is resolved.</p></div>}
    <details className="workspace-details"><summary>Workspace details</summary><dl><div><dt>Version</dt><dd>{repository.generation ?? 'Not available'}</dd></div><div><dt>Storage connection</dt><dd>{repository.backendHealth === 'unknown' ? 'Not checked' : repository.backendHealth.replaceAll('_', ' ')}</dd></div><div><dt>Workspace ID</dt><dd className="number">{repository.id}</dd></div>{repository.volumeMode && <div><dt>Volume</dt><dd>{repository.volumeMode === 'managed' ? 'Managed workspace' : 'Immutable workspace'}</dd></div>}</dl></details>
  </section>;
}

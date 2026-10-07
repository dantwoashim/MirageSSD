import { Broom, FolderOpen } from '@phosphor-icons/react';
import { useState } from 'react';
import type { ServiceClient } from '../../api/client';
import { formatBytes } from '../../format';
import type { RepositoryState } from '../../models';
import { connectionStatus, driveLetter, record, uploadStatus } from '../../presentation';
import { Button, Card, StatusPill, cx } from '../../ui';
import { Meter } from '../../ui/Meter';
import { CacheLocation, FloorSentence } from './CacheLocation';
import { OffloadPanel } from './OffloadPanel';
import { PinnedFolders } from './PinnedFolders';
import { RemoveDrive } from './RemoveDrive';

/** Everyday panel for a managed Drive-backed volume: status, upload
 * progress, storage split, cache location, and the common tasks. */
export function DrivePanel({
  client,
  repository,
  busy,
  onToggleMount,
  onChanged,
}: {
  client: ServiceClient;
  repository: RepositoryState;
  busy: boolean;
  onToggleMount: () => void;
  onChanged: (notice?: string, error?: string) => void;
}) {
  const [working, setWorking] = useState<string>();
  const letter = driveLetter(repository);
  const connection = connectionStatus(repository);
  const upload = uploadStatus(repository);
  const disabled = busy || Boolean(working);
  const physical = repository.physicalBytes ?? 0;
  const diskTotal = repository.cacheDiskTotalBytes ?? undefined;
  const diskFree = repository.cacheDiskFreeBytes ?? undefined;

  const act = async (label: string, run: () => Promise<unknown>, notice: string) => {
    setWorking(label);
    try {
      await run();
      onChanged(notice);
    } catch (failure) {
      onChanged(undefined, failure instanceof Error ? failure.message : String(failure));
    } finally {
      setWorking(undefined);
    }
  };

  const reclaim = async () => {
    const result = await client.diskReclaimNow();
    const freed = record(result) && typeof result.reclaimed_bytes === 'number' ? result.reclaimed_bytes : 0;
    onChanged(
      freed > 0
        ? `Freed ${formatBytes(freed)} of files that are already in Google Drive.`
        : 'Nothing to free right now — everything local is still in use or waiting to upload.',
    );
  };

  return (
    <div className="grid gap-4">
      <Card padding="lg" aria-label={`${repository.name} drive`}>
        <div className="flex flex-wrap items-center gap-4">
          <span
            className="grid size-14 shrink-0 place-items-center rounded-xl bg-accent-soft font-display text-xl font-semibold text-accent"
            aria-hidden="true"
          >
            {letter ? `${letter}:` : <FolderOpen size={22} weight="duotone" />}
          </span>
          <div className="min-w-0 flex-1">
            <h2 className="section-title text-lg">{repository.name}</h2>
            <div className="mt-1.5">
              <StatusPill tone={connection.tone} label={connection.label} />
            </div>
          </div>
          <div className="flex items-center gap-2">
            {letter && repository.mounted && (
              <Button
                icon={<FolderOpen size={16} />}
                disabled={disabled}
                onClick={() => void act('Open', () => client.openExplorer(letter), '')}
              >
                Open in Explorer
              </Button>
            )}
            <Button variant="secondary" disabled={disabled} onClick={onToggleMount}>
              {repository.mounted ? 'Disconnect' : 'Connect'}
            </Button>
          </div>
        </div>
        <div className="mt-4">
          <p className={cx('text-xs', upload.tone === 'busy' ? 'text-accent' : 'text-fg-muted')} role={upload.tone === 'busy' ? 'status' : undefined}>
            {upload.label}
          </p>
          {upload.tone === 'busy' && (
            <div className="indeterminate-bar mt-2" aria-hidden="true">
              <span />
            </div>
          )}
        </div>
      </Card>

      <Card title={diskTotal != null && diskFree != null ? `Storage on ${repository.cacheDiskRoot}` : 'Storage'} padding="lg">
        {diskTotal != null && diskFree != null ? (
          <>
            <Meter
              total={diskTotal}
              label={`${repository.cacheDiskRoot}: ${formatBytes(diskTotal - diskFree)} used of ${formatBytes(diskTotal)}`}
              segments={[
                { value: physical, tone: 'accent', label: 'MirageSSD on this PC' },
                { value: Math.max(0, diskTotal - diskFree - physical), tone: 'neutral', label: 'Other files' },
                { value: diskFree, tone: 'track', label: 'Free', bar: false },
              ]}
            />
            {repository.cacheDiskFloorBytes != null && (
              <p className="mt-2.5 text-xs text-fg-subtle">Always keeps {formatBytes(repository.cacheDiskFloorBytes)} free</p>
            )}
          </>
        ) : (
          <dl className="grid gap-4 sm:grid-cols-2">
            <div>
              <dt className="text-xs text-fg-muted">On this PC</dt>
              <dd className="number mt-1 text-lg text-fg">{formatBytes(physical)}</dd>
            </div>
            <div>
              <dt className="text-xs text-fg-muted">Waiting to upload</dt>
              <dd className="number mt-1 text-lg text-fg">{formatBytes(repository.pendingBytes ?? null)}</dd>
            </div>
          </dl>
        )}
        <div className="mt-4 border-t border-line pt-4">
          {repository.cacheDiskRoot ? (
            <CacheLocation client={client} repository={repository} busy={disabled} onChanged={onChanged} />
          ) : (
            <FloorSentence client={client} />
          )}
        </div>
      </Card>

      {repository.mounted && (
        <div className="grid gap-4 md:grid-cols-2">
          <Card title="Keep folders on this PC" description="Pinned folders stay downloaded on this PC so they open fast.">
            <PinnedFolders client={client} repository={repository} busy={disabled} onChanged={onChanged} />
          </Card>
          <Card title="Move a folder into this drive">
            <OffloadPanel client={client} repository={repository} busy={disabled} onChanged={onChanged} />
          </Card>
          <Card
            title="Free up space"
            description="Remove local copies of files that are already in Google Drive. They download again when you open them."
          >
            <Button variant="secondary" icon={<Broom size={15} />} disabled={disabled} onClick={() => void act('Free space', reclaim, '')}>
              Free up space
            </Button>
          </Card>
        </div>
      )}

      <RemoveDrive client={client} repository={repository} busy={disabled} act={act} />

      {working && (
        <p className="flex items-center gap-2 text-xs text-fg-muted" role="status">
          <span className="busy-dot text-accent" aria-hidden="true" />
          {working}…
        </p>
      )}
    </div>
  );
}

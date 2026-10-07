import { CaretRight, Database, HardDrives, Plus } from '@phosphor-icons/react';
import { formatBytes } from '../format';
import type { RepositoryState } from '../models';
import { connectionStatus, driveLetter, isManagedDrive } from '../presentation';
import { Button, EmptyState, StatusPill, cx } from '../ui';

export function Dashboard({
  repositories,
  selectedId,
  onSelect,
  busy = false,
  onSetup,
}: {
  repositories: RepositoryState[];
  selectedId?: string;
  onSelect: (repositoryId: string) => void;
  busy?: boolean;
  onSetup?: () => void;
}) {
  if (repositories.length === 0) {
    return (
      <EmptyState
        icon={<HardDrives size={24} weight="duotone" />}
        title="Create your first drive"
        body="Sign in with Google and MirageSSD adds a drive to File Explorer. Files you use stay on this PC for speed and are encrypted before they're uploaded to your Google Drive."
        actions={
          <Button icon={<CaretRight size={15} />} onClick={onSetup}>
            Create your drive
          </Button>
        }
      />
    );
  }

  return (
    <ul className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3" aria-label="Your drives">
      {repositories.map((repository, index) => {
        const status = connectionStatus(repository);
        const letter = isManagedDrive(repository) ? driveLetter(repository) : undefined;
        return (
          <li key={repository.id} className="view-in" style={{ animationDelay: `${Math.min(index, 5) * 45}ms` }}>
            <button
              type="button"
              onClick={() => onSelect(repository.id)}
              disabled={busy}
              aria-pressed={selectedId === repository.id}
              className={cx(
                'grid h-full w-full content-start gap-2.5 rounded-xl border p-3.5 text-left transition-colors duration-150 ease-standard hover:border-line-strong',
                selectedId === repository.id ? 'border-accent/40 bg-accent-soft' : 'border-line bg-surface',
              )}
            >
              {letter ? (
                <span className="grid size-11 place-items-center rounded-[10px] bg-accent-soft font-display text-[15px] font-semibold text-accent" aria-hidden="true">
                  {letter}:
                </span>
              ) : (
                <span className="grid size-11 place-items-center rounded-[10px] border border-line bg-surface-2 text-fg-muted" aria-hidden="true">
                  <Database size={20} weight="duotone" />
                </span>
              )}
              <span className="min-w-0">
                <span className="block truncate text-[13px] font-semibold text-fg">{repository.name}</span>
                <StatusPill tone={status.tone} label={status.label} className="mt-1.5" />
                {(repository.pendingBytes ?? 0) > 0 && (
                  <span className="mt-1 block text-xs text-warn">Uploading {formatBytes(repository.pendingBytes ?? null)}</span>
                )}
              </span>
            </button>
          </li>
        );
      })}
      <li className="view-in" style={{ animationDelay: `${Math.min(repositories.length, 5) * 45}ms` }}>
        <button
          type="button"
          onClick={onSetup}
          className="grid h-full min-h-[116px] w-full place-content-center justify-items-center gap-1.5 rounded-xl border border-dashed border-line-strong text-fg-muted transition-colors duration-150 ease-standard hover:border-accent/60 hover:text-accent"
        >
          <Plus size={18} aria-hidden="true" />
          <span className="text-[13px] font-medium">Add a drive</span>
        </button>
      </li>
    </ul>
  );
}

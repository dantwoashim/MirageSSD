import { useState } from 'react';
import type { ServiceClient } from '../../api/client';
import { formatBytes } from '../../format';
import { useBackgroundJob } from '../../hooks/useBackgroundJob';
import type { RepositoryState, VolumeOffloadStatus } from '../../models';
import { Button, Input, Spinner } from '../../ui';

/** Move a folder from a local disk into this drive: every file is copied,
 * read back through the drive and hash-compared, Drive publication is
 * awaited, and only then is the local copy deleted (if asked). */
export function OffloadPanel({
  client,
  repository,
  busy,
  onChanged,
}: {
  client: ServiceClient;
  repository: RepositoryState;
  busy: boolean;
  onChanged: (notice?: string, error?: string) => void;
}) {
  const [source, setSource] = useState('');
  const [deleteSource, setDeleteSource] = useState(false);
  const job = useBackgroundJob<VolumeOffloadStatus>({
    fetchStatus: () => client.volumeOffloadStatus(),
    intervalMs: 1500,
    onSettled: (next) => {
      if (next.done && (next.failures?.length ?? 0) === 0) {
        onChanged(
          next.source_deleted
            ? `Moved ${next.files ?? 0} files (${formatBytes(next.bytes ?? 0)}) into the drive, verified in Google Drive, and freed the local copy.`
            : `Copied and verified ${next.verified ?? 0} of ${next.files ?? 0} files into the drive${next.published ? ' — all published to Google Drive.' : ' — still uploading to Google Drive.'}`,
        );
      } else {
        onChanged(
          undefined,
          next.error ??
            `Offload finished with ${next.failures?.length ?? 0} file(s) that could not be verified; the source folder was left untouched.`,
        );
      }
    },
  });

  const start = async () => {
    if (!source.trim()) return;
    try {
      await job.start(() =>
        client.volumeOffload({ repository_id: repository.id, source: source.trim(), delete_source: deleteSource }),
      );
    } catch (failure) {
      onChanged(undefined, failure instanceof Error ? failure.message : String(failure));
    }
  };

  const disabled = busy || job.running;
  return (
    <div>
      <p className="mb-3 text-[13px] leading-relaxed text-fg-muted">
        Every file is copied, read back through the drive and compared, and the drive uploads it to Google Drive. Nothing
        local is deleted unless you tick the box — and then only after every file is verified and uploaded.
      </p>
      <form
        className="grid gap-2.5"
        onSubmit={(event) => {
          event.preventDefault();
          void start();
        }}
      >
        <label>
          <span className="sr-only">Folder to move into this drive</span>
          <Input
            value={source}
            onChange={(event) => setSource(event.target.value)}
            placeholder="Folder to move, e.g. D:\Videos\2023"
            disabled={disabled}
          />
        </label>
        <label className="flex items-center gap-2 text-[13px] text-fg-muted">
          <input
            type="checkbox"
            checked={deleteSource}
            onChange={(event) => setDeleteSource(event.target.checked)}
            disabled={disabled}
            className="accent-accent"
          />
          Delete the local copy once everything is verified in Google Drive
        </label>
        <div>
          <Button type="submit" variant="secondary" size="sm" disabled={disabled || !source.trim()}>
            Move folder
          </Button>
        </div>
      </form>
      {job.running && (
        <p className="mt-3 flex items-center gap-2 text-xs text-accent" role="status">
          <Spinner size={13} /> {job.status?.step ?? 'Working'}…
        </p>
      )}
      {job.status && !job.status.in_flight && (job.status.failures?.length ?? 0) > 0 && (
        <ul className="mt-3 divide-y divide-line border-t border-line text-[13px]" aria-label="Files that could not be verified">
          {job.status.failures!.slice(0, 8).map((failure) => (
            <li key={failure} className="py-1.5">
              <span>{failure}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

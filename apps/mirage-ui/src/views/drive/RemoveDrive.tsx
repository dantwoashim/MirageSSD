import { useState } from 'react';
import type { ServiceClient } from '../../api/client';
import { formatBytes } from '../../format';
import type { RepositoryState } from '../../models';
import { Button, Card } from '../../ui';

/** The dangerous action lives at the bottom of the drive panel, behind an
 * explicit inline confirmation. */
export function RemoveDrive({
  client,
  repository,
  busy,
  act,
}: {
  client: ServiceClient;
  repository: RepositoryState;
  busy: boolean;
  act: (label: string, run: () => Promise<unknown>, notice: string) => Promise<void>;
}) {
  const [confirming, setConfirming] = useState(false);
  const [discard, setDiscard] = useState(false);
  const pending = repository.pendingBytes ?? 0;

  return (
    <Card className="border-danger/40" aria-label="Danger zone">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div className="min-w-0">
          <h2 className="section-title text-danger">Remove from this PC</h2>
          <p className="mt-1 max-w-[60ch] text-xs leading-relaxed text-fg-muted">
            Detaches this drive from MirageSSD. Files already in Google Drive stay there.
          </p>
        </div>
        {!confirming && (
          <Button variant="danger" size="sm" disabled={busy} onClick={() => setConfirming(true)}>
            Remove from this PC
          </Button>
        )}
      </div>
      {confirming && (
        <div className="mt-4 rounded-xl border border-danger/40 bg-danger-soft p-4" role="alertdialog" aria-label={`Remove ${repository.name} from this PC`}>
          <p className="text-[13px] leading-relaxed">
            Remove <strong>{repository.name}</strong> from this PC? Everything already on Google Drive stays there —
            nothing on Drive is deleted.
            {pending > 0
              ? ` ${formatBytes(pending)} have not finished uploading and would be discarded from this PC.`
              : ' All uploads have finished.'}
          </p>
          {pending > 0 && (
            <label className="mt-3 flex items-center gap-2 text-[13px]">
              <input type="checkbox" checked={discard} onChange={(event) => setDiscard(event.target.checked)} className="accent-accent" />
              Discard the {formatBytes(pending)} that haven't uploaded
            </label>
          )}
          <div className="mt-4 flex flex-wrap gap-2">
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => { setConfirming(false); setDiscard(false); }}>
              Keep this drive
            </Button>
            <Button
              variant="danger"
              size="sm"
              disabled={busy || (pending > 0 && !discard)}
              onClick={() => void act('Remove', async () => {
                await client.repositoryUnregister(repository.id, repository.mounted, discard);
                setConfirming(false);
                setDiscard(false);
              }, `${repository.name} was removed from this PC. Its files stay in your Drive.`)}
            >
              Remove from this PC
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}

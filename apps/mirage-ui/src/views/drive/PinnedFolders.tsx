import { PushPin } from '@phosphor-icons/react';
import { useEffect, useState } from 'react';
import type { ServiceClient } from '../../api/client';
import type { RepositoryState } from '../../models';
import { Button, Input } from '../../ui';

function parsePins(value: unknown): string[] {
  return Array.isArray((value as { pins?: unknown }).pins)
    ? (value as { pins: { path?: string }[] }).pins.map((pin) => pin.path ?? '').filter(Boolean)
    : [];
}

/** The pinned-folder list loads once when the drive panel mounts; pinning
 * and unpinning refresh it in place. */
export function PinnedFolders({
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
  const [pinPath, setPinPath] = useState('');
  const [pins, setPins] = useState<string[]>();
  const [working, setWorking] = useState(false);
  const disabled = busy || working;

  useEffect(() => {
    let stopped = false;
    void client
      .pins(repository.id)
      .then((value) => {
        if (!stopped) setPins(parsePins(value));
      })
      .catch(() => {});
    return () => {
      stopped = true;
    };
  }, [client, repository.id]);

  const run = async (work: () => Promise<unknown>, notice: string) => {
    setWorking(true);
    try {
      await work();
      setPins(parsePins(await client.pins(repository.id)));
      onChanged(notice);
    } catch (failure) {
      onChanged(undefined, failure instanceof Error ? failure.message : String(failure));
    } finally {
      setWorking(false);
    }
  };

  return (
    <div>
      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          const path = pinPath.trim();
          if (!path) return;
          setPinPath('');
          void run(() => client.pin(repository.id, path), `Pinned ${path} — it stays on this PC.`);
        }}
      >
        <label className="flex-1">
          <span className="sr-only">Folder to keep on this PC</span>
          <Input
            value={pinPath}
            onChange={(event) => setPinPath(event.target.value)}
            placeholder="Folder to keep, e.g. /photos"
            disabled={disabled}
          />
        </label>
        <Button type="submit" variant="secondary" size="sm" icon={<PushPin size={14} />} disabled={disabled || !pinPath.trim()}>
          Keep on this PC
        </Button>
      </form>
      {pins === undefined ? (
        <p className="mt-3 text-xs text-fg-subtle">Checking pinned folders…</p>
      ) : pins.length > 0 ? (
        <ul className="mt-3 divide-y divide-line border-t border-line" aria-label="Pinned folders">
          {pins.map((path) => (
            <li key={path} className="flex items-center justify-between gap-3 py-1.5 text-[13px]">
              <span className="min-w-0 truncate">{path}</span>
              <Button
                variant="ghost"
                size="sm"
                disabled={disabled}
                onClick={() => void run(() => client.unpin(repository.id, path), `Unpinned ${path}.`)}
              >
                Remove
              </Button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="mt-3 text-xs text-fg-subtle">No pinned folders yet.</p>
      )}
    </div>
  );
}

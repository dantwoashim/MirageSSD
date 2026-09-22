import { ArrowSquareOut, Broom, FolderOpen, Info, PushPin, SignOut } from '@phosphor-icons/react';
import { useState } from 'react';
import type { ServiceClient } from '../api/client';
import type { RepositoryState } from '../models';
import { formatBytes } from '../components/CapsuleBreakdown';

/** Card shown for a managed Drive-backed volume: letter, staged/pending
 * bytes, pins, and the everyday actions a drive owner needs. */
export function DriveCard({
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
  const [pinPath, setPinPath] = useState('');
  const [pins, setPins] = useState<string[]>();
  const [working, setWorking] = useState<string>();
  const letter = repository.mountPath?.replace(/[:\\].*$/, '') ?? repository.mountPath;

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

  const loadPins = () =>
    act('Pins', async () => {
      const value = await client.pins(repository.id);
      const list = Array.isArray((value as { pins?: unknown }).pins)
        ? (value as { pins: { path?: string }[] }).pins.map((pin) => pin.path ?? '').filter(Boolean)
        : [];
      setPins(list);
    }, '');

  return (
    <section className="drive-card" aria-label={`${repository.name} drive options`}>
      <div className="drive-card-row">
        <div>
          <h3>{letter ? `${letter}: — ${repository.name}` : repository.name}</h3>
          <p>
            {repository.mounted
              ? `${formatBytes(repository.physicalBytes ?? 0)} staged locally · ${formatBytes(repository.pendingBytes ?? 0)} waiting to sync`
              : 'Not connected. Connect to browse it in File Explorer.'}
          </p>
        </div>
        <div className="button-row">
          {letter && repository.mounted && (
            <button className="secondary-button" disabled={busy} onClick={() => void act('Open', () => client.openExplorer(letter), '')}>
              <FolderOpen size={16} />Open
            </button>
          )}
          <button className="secondary-button" disabled={busy || Boolean(working)} onClick={onToggleMount}>
            <ArrowSquareOut size={16} />{repository.mounted ? 'Unmount' : 'Mount'}
          </button>
        </div>
      </div>

      {repository.mounted && (
        <div className="drive-card-tools">
          <form
            className="pin-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (!pinPath.trim()) return;
              void act('Pin', () => client.pin(repository.id, pinPath.trim()), `Pinned ${pinPath.trim()} — it stays on this device.`);
              setPinPath('');
            }}
          >
            <label>
              <span className="sr-only">Folder to keep on this device</span>
              <input
                value={pinPath}
                onChange={(event) => setPinPath(event.target.value)}
                placeholder="Folder to keep offline, e.g. /photos"
                disabled={busy || Boolean(working)}
              />
            </label>
            <button type="submit" className="quiet-button" disabled={busy || Boolean(working) || !pinPath.trim()}>
              <PushPin size={15} />Keep on this device
            </button>
          </form>
          <div className="button-row">
            <button className="quiet-button" disabled={busy || Boolean(working)} onClick={() => void loadPins()}>
              Pinned folders{pins ? ` (${pins.length})` : ''}
            </button>
            <button className="quiet-button" disabled={busy || Boolean(working)}
              onClick={() => void act('Free space', () => client.diskReclaimNow(), 'MirageSSD freed the space it safely could.')}>
              <Broom size={15} />Free up space
            </button>
            <button className="quiet-button" disabled={busy || Boolean(working)}
              onClick={() => void act('Sign out', () => client.driveLogout(), 'Signed out of Google Drive.')}>
              <SignOut size={15} />Sign out
            </button>
          </div>
          {pins && pins.length > 0 && (
            <ul className="pin-list" aria-label="Pinned folders">
              {pins.map((path) => (
                <li key={path}>
                  <span>{path}</span>
                  <button className="quiet-button" disabled={busy || Boolean(working)}
                    onClick={() => void act('Unpin', () => client.unpin(repository.id, path).then(() => loadPinsQuiet()), `Unpinned ${path}.`)}>
                    Remove
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {working && <p className="text-xs text-zinc-500" role="status"><Info size={14} /> {working}…</p>}
    </section>
  );

  async function loadPinsQuiet() {
    const value = await client.pins(repository.id);
    const list = Array.isArray((value as { pins?: unknown }).pins)
      ? (value as { pins: { path?: string }[] }).pins.map((pin) => pin.path ?? '').filter(Boolean)
      : [];
    setPins(list);
  }
}

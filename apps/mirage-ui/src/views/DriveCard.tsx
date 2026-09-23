import { ArrowSquareOut, Broom, FolderOpen, HardDrives, Info, PushPin, SpinnerGap } from '@phosphor-icons/react';
import { useEffect, useState } from 'react';
import type { ServiceClient } from '../api/client';
import type { DiskInfo, RepositoryState, VolumeOffloadStatus, VolumeSetCacheStatus } from '../models';
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
  const [confirmingRemove, setConfirmingRemove] = useState(false);
  const [discard, setDiscard] = useState(false);
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
          {(repository.pendingBytes ?? 0) > 0 && (
            <p className="upload-live" role="status">
              <SpinnerGap size={13} className="refreshing" /> Uploading · {formatBytes(repository.pendingBytes ?? 0)} remaining
            </p>
          )}
          {repository.mounted && (() => {
            const staged = repository.physicalBytes ?? 0;
            const pending = repository.pendingBytes ?? 0;
            const published = repository.publishedBytes ?? 0;
            const total = Math.max(1, staged + pending + published);
            return (
              <div className="usage-bar" role="img"
                aria-label={`${formatBytes(staged)} on this device, ${formatBytes(pending)} waiting to upload`}>
                <span className="usage-seg usage-local" style={{ width: `${(staged / total) * 100}%` }} />
                <span className="usage-seg usage-pending" style={{ width: `${(pending / total) * 100}%` }} />
              </div>
            );
          })()}
          {repository.cacheDiskRoot ? (
            <CacheLocation client={client} repository={repository} busy={busy || Boolean(working)} onChanged={onChanged} />
          ) : (
            <FloorSentence client={client} />
          )}
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
          </div>
          <OffloadPanel client={client} repository={repository} busy={busy || Boolean(working)} onChanged={onChanged} />
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
      <div className="button-row">
        {!confirmingRemove ? (
          <button className="quiet-button" disabled={busy || Boolean(working)} onClick={() => setConfirmingRemove(true)}>
            Remove from this PC
          </button>
        ) : (
          <div className="confirm-panel" role="alertdialog" aria-label={`Remove ${repository.name} from this PC`}>
            <p>
              Remove <strong>{repository.name}</strong> from this PC?
              Everything already on Google Drive stays there — nothing on Drive is deleted.
              {(repository.pendingBytes ?? 0) > 0
                ? ` ${formatBytes(repository.pendingBytes ?? 0)} have not finished uploading and would be discarded from this PC.`
                : ' All uploads have finished.'}
            </p>
            {(repository.pendingBytes ?? 0) > 0 && (
              <label className="confirm-check">
                <input type="checkbox" checked={discard} onChange={(event) => setDiscard(event.target.checked)} />
                Discard the {formatBytes(repository.pendingBytes ?? 0)} that haven't uploaded
              </label>
            )}
            <div className="button-row">
              <button className="quiet-button" disabled={busy || Boolean(working)} onClick={() => { setConfirmingRemove(false); setDiscard(false); }}>
                Keep this drive
              </button>
              <button className="secondary-button" disabled={busy || Boolean(working) || ((repository.pendingBytes ?? 0) > 0 && !discard)}
                onClick={() => void act('Remove', async () => {
                  await client.repositoryUnregister(repository.id, repository.mounted, discard);
                  setConfirmingRemove(false);
                  setDiscard(false);
                }, `${repository.name} was removed from this PC. Its files stay in your Drive.`)}>
                Remove from this PC
              </button>
            </div>
          </div>
        )}
      </div>
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

const GIB = 1 << 30;

/** Move a folder from a local disk into this drive: every file is copied,
 * read back through the drive and hash-compared, Drive publication is
 * awaited, and only then is the local copy deleted (if asked). */
function OffloadPanel({ client, repository, busy, onChanged }: {
  client: ServiceClient;
  repository: RepositoryState;
  busy: boolean;
  onChanged: (notice?: string, error?: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [source, setSource] = useState('');
  const [deleteSource, setDeleteSource] = useState(false);
  const [status, setStatus] = useState<VolumeOffloadStatus>();

  useEffect(() => {
    if (!status?.in_flight) return;
    const tick = async () => {
      try {
        const next = await client.volumeOffloadStatus();
        setStatus(next);
        if (next.in_flight) { window.setTimeout(() => void tick(), 1500); return; }
        if (next.done && (next.failures?.length ?? 0) === 0) {
          onChanged(next.source_deleted
            ? `Moved ${next.files ?? 0} files (${formatBytes(next.bytes ?? 0)}) into the drive, verified in Google Drive, and freed the local copy.`
            : `Copied and verified ${next.verified ?? 0} of ${next.files ?? 0} files into the drive${next.published ? ' — all published to Google Drive.' : ' — still uploading to Google Drive.'}`);
        } else {
          onChanged(undefined, next.error ?? `Offload finished with ${next.failures?.length ?? 0} file(s) that could not be verified; the source folder was left untouched.`);
        }
      } catch (failure) {
        setStatus({ in_flight: false, error: failure instanceof Error ? failure.message : String(failure) });
      }
    };
    window.setTimeout(() => void tick(), 1500);
  }, [status?.in_flight, client, onChanged]);

  const start = async () => {
    if (!source.trim()) return;
    try {
      await client.volumeOffload({ repository_id: repository.id, source: source.trim(), delete_source: deleteSource });
      setStatus({ in_flight: true, step: 'starting' });
    } catch (failure) {
      onChanged(undefined, failure instanceof Error ? failure.message : String(failure));
    }
  };

  const running = Boolean(status?.in_flight);
  return (
    <div className="offload-panel">
      {!open ? (
        <button className="quiet-button" disabled={busy || running} onClick={() => setOpen(true)}>
          <HardDrives size={15} />Offload a folder into this drive…
        </button>
      ) : (
        <form className="confirm-panel" role="dialog" aria-label="Offload a folder" onSubmit={(event) => { event.preventDefault(); void start(); }}>
          <p>
            Move a folder from a local disk into this drive. Every file is copied, read back through the drive and compared, and the drive
            uploads it to Google Drive. Nothing local is deleted unless you tick the box — and then only after every file is verified and uploaded.
          </p>
          <label>
            <span className="sr-only">Folder to offload</span>
            <input value={source} onChange={(event) => setSource(event.target.value)} placeholder="Folder to move, e.g. D:\Videos\2023" disabled={running} />
          </label>
          <label className="confirm-check">
            <input type="checkbox" checked={deleteSource} onChange={(event) => setDeleteSource(event.target.checked)} disabled={running} />
            Delete the local copy once everything is verified in Google Drive
          </label>
          <div className="button-row">
            <button type="button" className="quiet-button" disabled={running} onClick={() => setOpen(false)}>Close</button>
            <button type="submit" className="secondary-button" disabled={running || !source.trim()}>Offload</button>
          </div>
        </form>
      )}
      {running && <p className="upload-live" role="status"><SpinnerGap size={13} className="refreshing" /> {status?.step ?? 'Working'}…</p>}
      {status && !status.in_flight && (status.failures?.length ?? 0) > 0 && (
        <ul className="pin-list" aria-label="Files that could not be verified">
          {status.failures!.slice(0, 8).map((failure) => <li key={failure}><span>{failure}</span></li>)}
        </ul>
      )}
    </div>
  );
}

/** Where this drive keeps its local cache, how much room that disk has, the
 * floor guarding it — with the controls to move the cache to another disk
 * and to change the floor. */
function CacheLocation({ client, repository, busy, onChanged }: {
  client: ServiceClient;
  repository: RepositoryState;
  busy: boolean;
  onChanged: (notice?: string, error?: string) => void;
}) {
  const [disks, setDisks] = useState<DiskInfo[]>();
  const [choosing, setChoosing] = useState(false);
  const [target, setTarget] = useState('');
  const [move, setMove] = useState<VolumeSetCacheStatus>();
  const [editingFloor, setEditingFloor] = useState(false);
  const [floorGib, setFloorGib] = useState(Math.max(1, Math.round((repository.cacheDiskFloorBytes ?? 20 * GIB) / GIB)));
  const [floorError, setFloorError] = useState<string>();
  const diskRoot = repository.cacheDiskRoot ?? '';
  const free = repository.cacheDiskFreeBytes ?? undefined;
  const floor = repository.cacheDiskFloorBytes ?? undefined;
  const room = free !== undefined && floor !== undefined ? free - floor : undefined;

  useEffect(() => {
    if (!choosing || disks) return;
    void client.disks().then((payload) => {
      setDisks(payload.disks);
      setTarget(payload.disks.find((disk) => disk.volume_root !== diskRoot)?.volume_root ?? '');
    }).catch((failure) => onChanged(undefined, failure instanceof Error ? failure.message : String(failure)));
  }, [choosing, disks, client, diskRoot, onChanged]);

  // Poll the move until the host reports it finished.
  useEffect(() => {
    if (!move?.in_flight) return;
    const tick = async () => {
      try {
        const next = await client.volumeSetCacheStatus();
        setMove(next);
        if (next.in_flight) { window.setTimeout(() => void tick(), 1000); return; }
        if (next.done) onChanged(`Local cache moved to ${next.cache_disk_root ?? next.cache_root ?? 'the new disk'}${next.remounted ? ' and the drive is back' : ''}.`);
        else onChanged(undefined, next.error ?? 'The cache move did not finish.');
      } catch (failure) {
        setMove({ in_flight: false, error: failure instanceof Error ? failure.message : String(failure) });
      }
    };
    window.setTimeout(() => void tick(), 1000);
  }, [move?.in_flight, client, onChanged]);

  const startMove = async () => {
    if (!target || target === diskRoot) return;
    try {
      await client.volumeSetCache({ repository_id: repository.id, cache_disk: target });
      setMove({ in_flight: true, step: 'starting' });
      setChoosing(false);
    } catch (failure) {
      onChanged(undefined, failure instanceof Error ? failure.message : String(failure));
    }
  };

  const saveFloor = async () => {
    setFloorError(undefined);
    const bytes = Math.max(1, floorGib) * GIB;
    try {
      await client.diskFloorSet(diskRoot, bytes);
      setEditingFloor(false);
      onChanged(`MirageSSD now always keeps ${formatBytes(bytes)} free on ${diskRoot}.`);
    } catch (failure) {
      setFloorError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const moving = Boolean(move?.in_flight);
  return (
    <div className="cache-location">
      <p className="floor-sentence">
        <HardDrives size={14} /> Local cache on <strong>{diskRoot}</strong>
        {free !== undefined && ` · ${formatBytes(free)} free`}
        {floor !== undefined ? ` · always keeps ${formatBytes(floor)} free` : ' · no free-space floor'}
        {room !== undefined && room <= 0 && (
          <span className="text-amber-300"> · nothing more can be kept locally until {diskRoot} has more than {formatBytes(floor ?? 0)} free</span>
        )}
      </p>
      <div className="button-row">
        {!choosing ? (
          <button className="quiet-button" disabled={busy || moving} onClick={() => setChoosing(true)}>
            Move cache…
          </button>
        ) : (
          <div className="confirm-panel" role="dialog" aria-label="Move local cache">
            <p>Move the local cache to another disk. The drive disconnects briefly while files are copied and verified, then comes back.</p>
            <select aria-label="Target disk" value={target} onChange={(event) => setTarget(event.target.value)} disabled={moving}>
              {(disks ?? []).filter((disk) => disk.volume_root !== diskRoot).map((disk) => (
                <option key={disk.volume_root} value={disk.volume_root}>{disk.volume_root} — {formatBytes(disk.free_bytes)} free of {formatBytes(disk.total_bytes)}</option>
              ))}
            </select>
            <div className="button-row">
              <button className="quiet-button" disabled={moving} onClick={() => setChoosing(false)}>Cancel</button>
              <button className="secondary-button" disabled={moving || !target} onClick={() => void startMove()}>Move to {target || '…'}</button>
            </div>
          </div>
        )}
        {!editingFloor ? (
          <button className="quiet-button" disabled={busy || moving} onClick={() => setEditingFloor(true)}>
            Change floor
          </button>
        ) : (
          <form className="pin-form" onSubmit={(event) => { event.preventDefault(); void saveFloor(); }}>
            <label>
              <span className="sr-only">Always keep free on {diskRoot}, in GiB</span>
              <input type="number" min={1} max={65536} value={floorGib} onChange={(event) => setFloorGib(Math.max(1, Number(event.target.value)))} aria-label="Free-space floor in GiB" />
            </label>
            <button type="submit" className="quiet-button">Keep {floorGib} GiB free on {diskRoot}</button>
            <button type="button" className="quiet-button" onClick={() => setEditingFloor(false)}>Cancel</button>
          </form>
        )}
      </div>
      {floorError && <p className="text-xs text-rose-300" role="alert">{floorError}</p>}
      {moving && <p className="upload-live" role="status"><SpinnerGap size={13} className="refreshing" /> {move?.step ?? 'Moving'}…</p>}
    </div>
  );
}

/** One-line floor status under the drive card: which disk is protected and
 * how much is always kept free (fetched once per card mount). */
function FloorSentence({ client }: { client: ServiceClient }) {
  const [text, setText] = useState<string>();
  useEffect(() => {
    void client.diskStatus().then((status) => {
      const floors = (status as { floors?: { volume_root: string; floor_bytes: number }[] }).floors ?? [];
      if (floors.length === 0) {
        setText('No free-space floor set — add one with mirage disk set-floor.');
      } else {
        const [first] = floors;
        setText(`Free-space protection: always keeps ${formatBytes(first.floor_bytes)} free on ${first.volume_root}.`);
      }
    }).catch(() => setText('Free-space floor status unavailable.'));
  });
  if (!text) return null;
  return <p className="floor-sentence">{text}</p>;
}

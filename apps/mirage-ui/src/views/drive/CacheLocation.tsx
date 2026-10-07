import { HardDrives } from '@phosphor-icons/react';
import { useEffect, useState } from 'react';
import type { ServiceClient } from '../../api/client';
import { formatBytes } from '../../format';
import { useBackgroundJob } from '../../hooks/useBackgroundJob';
import type { DiskInfo, RepositoryState, VolumeSetCacheStatus } from '../../models';
import { Button, Input, Select, Spinner, cx } from '../../ui';

const GIB = 1 << 30;

/** Where this drive keeps its local cache, how much room that disk has, the
 * floor guarding it — with the controls to move the cache to another disk
 * and to change the floor. */
export function CacheLocation({
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
  const [disks, setDisks] = useState<DiskInfo[]>();
  const [choosing, setChoosing] = useState(false);
  const [target, setTarget] = useState('');
  const [editingFloor, setEditingFloor] = useState(false);
  const [floorGib, setFloorGib] = useState(Math.max(1, Math.round((repository.cacheDiskFloorBytes ?? 20 * GIB) / GIB)));
  const [floorError, setFloorError] = useState<string>();
  const diskRoot = repository.cacheDiskRoot ?? '';
  const free = repository.cacheDiskFreeBytes ?? undefined;
  const floor = repository.cacheDiskFloorBytes ?? undefined;
  const room = free !== undefined && floor !== undefined ? free - floor : undefined;

  const move = useBackgroundJob<VolumeSetCacheStatus>({
    fetchStatus: () => client.volumeSetCacheStatus(),
    intervalMs: 1000,
    onSettled: (next) => {
      if (next.done) {
        onChanged(
          `Local cache moved to ${next.cache_disk_root ?? next.cache_root ?? 'the new disk'}${next.remounted ? ' and the drive is back' : ''}.`,
        );
      } else {
        onChanged(undefined, next.error ?? 'The cache move did not finish.');
      }
    },
  });

  useEffect(() => {
    if (!choosing || disks) return;
    void client
      .disks()
      .then((payload) => {
        setDisks(payload.disks);
        setTarget(payload.disks.find((disk) => disk.volume_root !== diskRoot)?.volume_root ?? '');
      })
      .catch((failure) => onChanged(undefined, failure instanceof Error ? failure.message : String(failure)));
  }, [choosing, disks, client, diskRoot, onChanged]);

  const startMove = async () => {
    if (!target || target === diskRoot) return;
    try {
      await move.start(() => client.volumeSetCache({ repository_id: repository.id, cache_disk: target }));
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

  const disabled = busy || move.running;
  return (
    <div>
      <p className="flex flex-wrap items-center gap-x-1.5 gap-y-1 text-xs text-fg-muted">
        <HardDrives size={14} aria-hidden="true" />
        <span>
          Local cache on <strong className="font-medium text-fg">{diskRoot}</strong>
          {free !== undefined && ` · ${formatBytes(free)} free`}
          {floor !== undefined ? ` · always keeps ${formatBytes(floor)} free` : ' · no free-space floor'}
        </span>
      </p>
      {room !== undefined && room <= 0 && (
        <p className="mt-1.5 text-xs text-warn" role="alert">
          Nothing more can be kept locally until {diskRoot} has more than {formatBytes(floor ?? 0)} free.
        </p>
      )}
      <div className="mt-2.5 flex flex-wrap gap-2">
        {!choosing ? (
          <Button variant="ghost" size="sm" disabled={disabled} onClick={() => setChoosing(true)}>
            Move cache…
          </Button>
        ) : (
          <div className="w-full rounded-xl border border-line bg-surface-2 p-3.5" role="dialog" aria-label="Move local cache">
            <p className="text-[13px] leading-relaxed text-fg-muted">
              Move the local cache to another disk. The drive disconnects briefly while files are copied and verified,
              then comes back.
            </p>
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <Select
                aria-label="Target disk"
                value={target}
                onChange={(event) => setTarget(event.target.value)}
                disabled={move.running}
                className="w-auto min-w-64"
              >
                {(disks ?? [])
                  .filter((disk) => disk.volume_root !== diskRoot)
                  .map((disk) => (
                    <option key={disk.volume_root} value={disk.volume_root}>
                      {disk.volume_root} — {formatBytes(disk.free_bytes)} free of {formatBytes(disk.total_bytes)}
                    </option>
                  ))}
              </Select>
              <Button variant="secondary" size="sm" disabled={move.running || !target} onClick={() => void startMove()}>
                Move to {target || '…'}
              </Button>
              <Button variant="ghost" size="sm" disabled={move.running} onClick={() => setChoosing(false)}>
                Cancel
              </Button>
            </div>
          </div>
        )}
        {!editingFloor ? (
          <Button variant="ghost" size="sm" disabled={disabled} onClick={() => setEditingFloor(true)}>
            Change free-space floor
          </Button>
        ) : (
          <form
            className="flex flex-wrap items-center gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void saveFloor();
            }}
          >
            <label>
              <span className="sr-only">Always keep free on {diskRoot}, in GiB</span>
              <Input
                type="number"
                min={1}
                max={65536}
                value={floorGib}
                onChange={(event) => setFloorGib(Math.max(1, Number(event.target.value)))}
                aria-label="Free-space floor in GiB"
                className="w-28"
              />
            </label>
            <Button type="submit" variant="secondary" size="sm">
              Keep {floorGib} GiB free on {diskRoot}
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setEditingFloor(false)}>
              Cancel
            </Button>
          </form>
        )}
      </div>
      {floorError && (
        <p className="mt-2 text-xs text-danger" role="alert">
          {floorError}
        </p>
      )}
      {move.running && (
        <p className={cx('mt-2.5 flex items-center gap-2 text-xs text-accent')} role="status">
          <Spinner size={13} /> {move.status?.step ?? 'Moving'}…
        </p>
      )}
    </div>
  );
}

/** One-line floor status under the storage card: which disk is protected and
 * how much is always kept free (fetched once per mount). */
export function FloorSentence({ client }: { client: ServiceClient }) {
  const [text, setText] = useState<string>();
  useEffect(() => {
    let stopped = false;
    void client
      .diskStatus()
      .then((status) => {
        if (stopped) return;
        const floors = (status as { floors?: { volume_root: string; floor_bytes: number }[] }).floors ?? [];
        if (floors.length === 0) {
          setText('No free-space floor set — add one with mirage disk set-floor.');
        } else {
          const [first] = floors;
          setText(`Free-space protection: always keeps ${formatBytes(first.floor_bytes)} free on ${first.volume_root}.`);
        }
      })
      .catch(() => {
        if (!stopped) setText('Free-space floor status unavailable.');
      });
    return () => {
      stopped = true;
    };
  }, [client]);
  if (!text) return null;
  return <p className="text-xs text-fg-muted">{text}</p>;
}

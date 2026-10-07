import { CaretRight, Check, CheckCircle, Copy, FolderOpen, GoogleLogo, HardDrives, PushPin } from '@phosphor-icons/react';
import { useEffect, useRef, useState } from 'react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';
import type { ServiceClient } from '../api/client';
import { formatBytes } from '../format';
import { useBackgroundJob } from '../hooks/useBackgroundJob';
import { usePolling } from '../hooks/usePolling';
import type { DisksPayload, VolumeCreateStatus } from '../models';
import { Button, Card, Field, Input, Notice, Select, Spinner, cx } from '../ui';

type Step = 'signin' | 'create' | 'done';

const GIB = 1 << 30;
const MIB = 1 << 20;
type Unit = 'GiB' | 'MiB';

function unitFactor(unit: Unit): number {
  return unit === 'GiB' ? GIB : MIB;
}

function Stepper({ step }: { step: Step }) {
  const items = [
    { id: 'signin', label: 'Sign in' },
    { id: 'create', label: 'Create' },
    { id: 'done', label: 'Ready' },
  ] as const;
  const active = items.findIndex((item) => item.id === step);
  return (
    <ol className="flex items-center gap-3 text-xs text-fg-subtle" aria-label="Setup progress">
      {items.map((item, index) => (
        <li key={item.id} className={cx('flex items-center gap-1.5', index === active && 'font-medium text-fg', index < active && 'text-ok')}>
          <span
            className={cx(
              'grid size-[18px] place-items-center rounded-full border text-[10px]',
              index === active && 'border-accent text-accent',
              index < active && 'border-ok bg-ok-soft text-ok',
              index > active && 'border-line-strong',
            )}
            aria-hidden="true"
          >
            {index < active ? <Check size={11} weight="bold" /> : index + 1}
          </span>
          {item.label}
          {index < items.length - 1 && <span className="ml-2 h-px w-5 bg-line-strong" aria-hidden="true" />}
        </li>
      ))}
    </ol>
  );
}

function ErrorNotice({ message }: { message: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Notice tone="danger" title="Something needs attention" className="mt-4">
      <p>{message}</p>
      <button
        type="button"
        className="mt-2 inline-flex items-center gap-1.5 text-xs font-medium text-fg-muted underline-offset-2 transition-colors duration-150 ease-standard hover:text-fg hover:underline"
        onClick={() => {
          void navigator.clipboard
            ?.writeText(message)
            .then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            })
            .catch(() => {});
        }}
      >
        <Copy size={13} aria-hidden="true" />
        {copied ? 'Copied' : 'Copy details'}
      </button>
    </Notice>
  );
}

export function SetupWizard({ client, account, onDone, autoCreate = false }: { client: ServiceClient; account: DriveAccount; onDone: () => void; autoCreate?: boolean }) {
  const acct = useDriveAccount(account);
  const [step, setStep] = useState<Step>('signin');
  const [disks, setDisks] = useState<DisksPayload>();
  const [disksLoading, setDisksLoading] = useState(true);
  const [letter, setLetter] = useState('');
  const [name, setName] = useState('MirageSSD');
  const [budgetValue, setBudgetValue] = useState(0);
  const [budgetUnit, setBudgetUnit] = useState<Unit>('GiB');
  const [floorValue, setFloorValue] = useState(0);
  const [floorUnit, setFloorUnit] = useState<Unit>('GiB');
  const [cacheDisk, setCacheDisk] = useState('');
  const [error, setError] = useState<string>();
  const [pinState, setPinState] = useState<'idle' | 'done' | 'failed'>('idle');
  // First drive on a fresh PC: create it as soon as sign-in finishes and the
  // defaults are known — no extra click. Happens at most once per mount.
  const autoStarted = useRef(false);

  const budgetBytes = budgetValue * unitFactor(budgetUnit);
  const floorBytes = floorValue * unitFactor(floorUnit);

  // Poll sign-in progress while on step 1; the shared account store holds
  // the authoritative state.
  usePolling(() => account.refresh(), { intervalMs: 1500, enabled: step === 'signin' });

  useEffect(() => {
    if (step === 'signin' && acct.phase === 'signed_in') setStep('create');
  }, [step, acct.phase]);

  // Load disk defaults once we reach step 2.
  useEffect(() => {
    if (step !== 'create') return;
    setDisksLoading(true);
    void client.disks().then((next) => {
      setDisks(next);
      setLetter(next.default_letter || 'M');
      setBudgetValue(Math.max(1, Math.round(next.default_budget_bytes / GIB)));
      const defaultDisk = next.default_cache_disk ?? next.state_volume?.volume_root ?? '';
      setCacheDisk(defaultDisk);
      const chosen = next.disks.find((disk) => disk.volume_root === defaultDisk) ?? next.state_volume;
      setFloorValue(chosen ? Math.max(1, Math.round(Math.max(chosen.total_bytes / 10, 20 * GIB) / GIB)) : 20);
      setDisksLoading(false);
    }).catch((failure) => {
      setDisksLoading(false);
      setError(failure instanceof Error ? failure.message : String(failure));
    });
  }, [step, client]);

  const createJob = useBackgroundJob<VolumeCreateStatus>({
    fetchStatus: () => client.volumeCreateStatus(),
    intervalMs: 1000,
    onSettled: (next) => {
      if (next.done) setStep('done');
      else if (next.error) setError(next.error);
    },
  });
  const createStatus = createJob.status;
  const creating = createJob.running;

  const startLogin = async () => {
    setError(undefined);
    try { await account.signIn(); } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const startCreate = async () => {
    setError(undefined);
    if (!name.trim()) { setError('Choose a name for your drive.'); return; }
    if (!/^[A-Za-z]:?$/.test(letter.trim())) { setError('Choose a single drive letter, for example M.'); return; }
    try {
      await createJob.start(() => client.volumeCreate({
        name: name.trim(),
        letter: letter.trim().replace(/:$/, ''),
        budget_bytes: budgetBytes,
        floor_bytes: floorBytes > 0 ? floorBytes : undefined,
        cache_disk: cacheDisk || undefined,
      }));
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  useEffect(() => {
    if (!autoCreate || autoStarted.current || step !== 'create' || disksLoading || !disks || !letter || budgetValue <= 0 || createStatus) return;
    autoStarted.current = true;
    void startCreate();
  // startCreate reads the current form values; the guards above make this run once.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoCreate, step, disksLoading, disks, letter, budgetValue, createStatus]);

  const pinToQuickAccess = async (driveLetter: string) => {
    try {
      await client.pinQuickAccess(driveLetter);
      setPinState('done');
    } catch {
      setPinState('failed');
    }
  };

  const phase = acct.phase === 'signing_in' ? 'in_flight' : acct.phase === 'error' ? 'failed' : 'idle';
  const failed = acct.phase === 'error' ? acct.error : undefined;
  const stateVolume = disks?.state_volume;
  // The disk that will physically hold the cache: the chosen one, else the state disk.
  const cacheVolume = disks?.disks.find((disk) => disk.volume_root === cacheDisk) ?? stateVolume;
  const leavesFree = cacheVolume ? Math.max(0, cacheVolume.free_bytes - floorBytes) : undefined;
  // Room MirageSSD may actually use locally right now: free space above the floor.
  const localRoom = cacheVolume ? cacheVolume.free_bytes - floorBytes : undefined;
  const floorBlocksWrites = localRoom !== undefined && localRoom <= 0;

  return (
    <section className="mx-auto w-full max-w-[640px]" aria-labelledby="setup-title">
      <Card padding="lg">
        <Stepper step={step} />
        <h2 id="setup-title" className="page-title mt-5 text-2xl">
          {step === 'signin' && 'Connect your Google account'}
          {step === 'create' && 'Create your drive'}
          {step === 'done' && 'Your drive is ready'}
        </h2>

        {step === 'signin' && <>
          <p className="mt-3 max-w-[58ch] text-[13px] leading-relaxed text-fg-muted">
            MirageSSD keeps a drive letter in Windows backed by your Google Drive. Sign in once — it reconnects automatically at every sign-in.
          </p>
          {(error || failed) && <ErrorNotice message={error ?? failed ?? 'Sign-in failed.'} />}
          <div className="mt-5">
            <Button icon={phase === 'in_flight' ? <Spinner size={15} /> : <GoogleLogo size={16} weight="bold" />} loading={false} onClick={() => void startLogin()} disabled={phase === 'in_flight'}>
              {phase === 'in_flight' ? 'Waiting for Google…' : 'Sign in with Google'}
            </Button>
          </div>
          {phase === 'in_flight' && <p className="mt-3 text-xs text-fg-subtle">Finish sign-in in the browser window that just opened, then come back here.</p>}
          <p className="mt-4 text-xs leading-relaxed text-fg-subtle">
            MirageSSD only gets access to the files it creates in your Google Drive, and encrypts file contents before uploading them.
          </p>
        </>}

        {step === 'create' && <>
          <p className="mt-3 max-w-[58ch] text-[13px] leading-relaxed text-fg-muted">
            Signed in{acct.email ? ` as ${acct.email}` : ''}. Choose a letter and how much local space MirageSSD may use for speed.{' '}
            <button
              type="button"
              className="text-accent underline-offset-2 transition-colors duration-150 ease-standard hover:underline disabled:opacity-50"
              disabled={creating}
              onClick={() => {
                setError(undefined);
                setStep('signin');
                void account.switchAccount().catch((failure) => {
                  setError(failure instanceof Error ? failure.message : String(failure));
                });
              }}
            >Use a different account</button>
          </p>
          {disksLoading && !disks ? (
            <div className="mt-5 grid gap-3" aria-busy="true" aria-label="Loading disk options">
              <span className="skeleton h-14" />
              <span className="skeleton h-14" />
              <span className="skeleton h-14" />
            </div>
          ) : (
          <div className="mt-5 grid gap-4">
            <div className="grid gap-4 sm:grid-cols-[minmax(0,10rem)_minmax(0,1fr)]">
              <Field label="Drive letter" htmlFor="setup-letter">
                <Select id="setup-letter" aria-label="Drive letter" value={letter} onChange={(event) => setLetter(event.target.value)} disabled={creating}>
                  {(disks?.free_letters?.length ? disks.free_letters : [letter || 'M']).map((free) => (
                    <option key={free} value={free}>{free}:</option>
                  ))}
                </Select>
              </Field>
              <Field label="Name" htmlFor="setup-name">
                <Input id="setup-name" value={name} maxLength={64} onChange={(event) => setName(event.target.value)} disabled={creating} />
              </Field>
            </div>
            {floorBlocksWrites && cacheVolume && (
              <Notice tone="warning">
                {cacheVolume.volume_root} only has {formatBytes(cacheVolume.free_bytes)} free, so with this floor MirageSSD can keep nothing on it and new files cannot be saved until you free space or lower the floor. Moving folders from {cacheVolume.volume_root} into this drive is how you get there.
              </Notice>
            )}
            <details className="rounded-xl border border-line bg-surface-2 px-4 py-3">
              <summary className="cursor-pointer text-[13px] font-medium text-fg-muted transition-colors duration-150 ease-standard hover:text-fg">
                Cache location and limits
              </summary>
              <div className="mt-4 grid gap-4">
                <Field label="Keep the local cache on" hint="Files you use stay on this disk for speed; everything is also in your Google Drive.">
                  <Select aria-label="Cache disk" value={cacheDisk} onChange={(event) => {
                    const next = event.target.value;
                    setCacheDisk(next);
                    const chosen = disks?.disks.find((disk) => disk.volume_root === next);
                    if (chosen) { setFloorUnit('GiB'); setFloorValue(Math.max(1, Math.round(Math.max(chosen.total_bytes / 10, 20 * GIB) / GIB))); }
                  }} disabled={creating}>
                    {(disks?.disks ?? []).map((disk) => (
                      <option key={disk.volume_root} value={disk.volume_root}>{disk.volume_root} — {formatBytes(disk.free_bytes)} free of {formatBytes(disk.total_bytes)}</option>
                    ))}
                  </Select>
                </Field>
                <Field
                  label="Local SSD budget"
                  hint={cacheVolume ? `On ${cacheVolume.volume_root} — ${formatBytes(cacheVolume.free_bytes)} free of ${formatBytes(cacheVolume.total_bytes)}` : undefined}
                >
                  <div className="flex items-center gap-2">
                    <Input type="number" min={1} max={4096} value={budgetValue} onChange={(event) => setBudgetValue(Math.max(1, Number(event.target.value)))} disabled={creating} aria-label="Local SSD budget" className="w-28" />
                    <Select aria-label="Budget unit" value={budgetUnit} onChange={(event) => setBudgetUnit(event.target.value as Unit)} disabled={creating} className="w-24">
                      <option value="GiB">GiB</option>
                      <option value="MiB">MiB</option>
                    </Select>
                  </div>
                </Field>
                <Field
                  label="Always keep free"
                  hint={`MirageSSD evicts cloud-backed data before ${cacheVolume?.volume_root ?? 'this disk'} fills past this point.${leavesFree !== undefined && !floorBlocksWrites ? ` Right now that leaves ~${formatBytes(leavesFree)} for MirageSSD to use locally.` : ''}`}
                >
                  <div className="flex items-center gap-2">
                    <Input type="number" min={1} max={4096} value={floorValue} onChange={(event) => setFloorValue(Math.max(1, Number(event.target.value)))} disabled={creating} aria-label="Free-space floor" className="w-28" />
                    <Select aria-label="Floor unit" value={floorUnit} onChange={(event) => setFloorUnit(event.target.value as Unit)} disabled={creating} className="w-24">
                      <option value="GiB">GiB</option>
                      <option value="MiB">MiB</option>
                    </Select>
                  </div>
                </Field>
              </div>
            </details>
          </div>
          )}
          {error && <ErrorNotice message={error} />}
          {creating && (
            <div className="mt-5" role="status">
              <p className="flex items-center gap-2 text-xs text-fg-muted"><Spinner size={13} />{createStatus?.step ?? 'Working'}…</p>
              <div className="indeterminate-bar mt-2" aria-hidden="true"><span /></div>
            </div>
          )}
          <div className="mt-5">
            <Button icon={<CaretRight size={15} />} onClick={() => void startCreate()} disabled={creating || (disksLoading && !disks)}>
              Create drive
            </Button>
          </div>
        </>}

        {step === 'done' && <>
          {createStatus?.drive_letter ? (
            <div className="mt-5 flex items-center gap-4 rounded-xl border border-line bg-surface-2 p-5" role="status">
              <span className="grid size-14 place-items-center rounded-xl bg-accent-soft font-display text-xl font-semibold text-accent">{createStatus.drive_letter}:</span>
              <span className="section-title">{createStatus.name ?? name}</span>
            </div>
          ) : (
            <div className="mt-5 flex items-center gap-3 text-fg" role="status">
              <CheckCircle size={20} weight="bold" className="text-ok" aria-hidden="true" />
              <span className="section-title">{createStatus?.name ?? name}</span>
            </div>
          )}
          <p className="mt-3 max-w-[58ch] text-[13px] leading-relaxed text-fg-muted">
            Live in File Explorer. Files you use stay on this PC for speed and upload to your Google Drive in the background.
          </p>
          <div className="mt-5 flex flex-wrap items-center gap-2">
            {createStatus?.drive_letter && (
              <Button icon={<FolderOpen size={16} />} onClick={() => void client.openExplorer(createStatus.drive_letter!).catch(() => {})}>
                Open {createStatus.drive_letter}: in Explorer
              </Button>
            )}
            {createStatus?.drive_letter && pinState !== 'done' && (
              <Button variant="secondary" icon={<PushPin size={15} />} onClick={() => void pinToQuickAccess(createStatus.drive_letter!)}>
                {pinState === 'failed' ? 'Try pinning again' : 'Pin to Quick Access'}
              </Button>
            )}
            {pinState === 'done' && <span className="text-xs text-ok" role="status">Pinned to Quick Access</span>}
            <Button variant="ghost" onClick={onDone}>Done</Button>
          </div>
        </>}
      </Card>
    </section>
  );
}

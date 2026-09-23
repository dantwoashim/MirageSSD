import { CaretRight, Check, CheckCircle, Copy, FolderOpen, GoogleLogo, HardDrives, Info, PushPin, SpinnerGap } from '@phosphor-icons/react';
import { useEffect, useRef, useState } from 'react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';
import type { ServiceClient } from '../api/client';
import type { DisksPayload, VolumeCreateStatus } from '../models';
import { formatBytes } from '../components/CapsuleBreakdown';

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
    { id: 'done', label: 'Done' },
  ] as const;
  const active = items.findIndex((item) => item.id === step);
  return (
    <ol className="stepper" aria-label="Setup progress">
      {items.map((item, index) => (
        <li key={item.id} className={index < active ? 'done' : index === active ? 'active' : ''}>
          <span className="step-dot" aria-hidden="true">{index < active ? <Check size={12} weight="bold" /> : index + 1}</span>
          {item.label}
        </li>
      ))}
    </ol>
  );
}

function ErrorCard({ message }: { message: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="feedback feedback-error error-card" role="alert">
      <Info size={20} />
      <div>
        <strong>Something needs attention</strong>
        <p>{message}</p>
        <button
          className="quiet-button error-copy"
          onClick={() => {
            void navigator.clipboard?.writeText(message).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            }).catch(() => {});
          }}
        >
          <Copy size={14} />{copied ? 'Copied' : 'Copy details'}
        </button>
      </div>
    </div>
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
  const [createStatus, setCreateStatus] = useState<VolumeCreateStatus>();
  const [error, setError] = useState<string>();
  const [pinState, setPinState] = useState<'idle' | 'done' | 'failed'>('idle');
  const polling = useRef(true);
  // First drive on a fresh PC: create it as soon as sign-in finishes and the
  // defaults are known — no extra click. Happens at most once per mount.
  const autoStarted = useRef(false);

  const budgetBytes = budgetValue * unitFactor(budgetUnit);
  const floorBytes = floorValue * unitFactor(floorUnit);

  // Poll sign-in progress while on step 1; the shared account store holds
  // the authoritative state.
  useEffect(() => {
    if (step !== 'signin') return;
    polling.current = true;
    const tick = async () => {
      await account.refresh();
      if (!polling.current) return;
      window.setTimeout(() => void tick(), 1500);
    };
    void tick();
    return () => { polling.current = false; };
  }, [step, account]);

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

  // Poll volume creation progress while in flight.
  useEffect(() => {
    if (!createStatus?.in_flight) return;
    const tick = async () => {
      try {
        const next = await client.volumeCreateStatus();
        setCreateStatus(next);
        if (!next.in_flight) return;
        window.setTimeout(() => void tick(), 1000);
      } catch (failure) {
        setCreateStatus({ in_flight: false, done: false, error: failure instanceof Error ? failure.message : String(failure) });
      }
    };
    window.setTimeout(() => void tick(), 1000);
  }, [createStatus?.in_flight, client]);

  useEffect(() => {
    if (createStatus && !createStatus.in_flight) {
      if (createStatus.done) setStep('done');
      else if (createStatus.error) setError(createStatus.error);
    }
  }, [createStatus]);

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
      await client.volumeCreate({
        name: name.trim(),
        letter: letter.trim().replace(/:$/, ''),
        budget_bytes: budgetBytes,
        floor_bytes: floorBytes > 0 ? floorBytes : undefined,
        cache_disk: cacheDisk || undefined,
      });
      setCreateStatus({ in_flight: true, step: 'starting' });
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
  const creating = Boolean(createStatus?.in_flight);
  const stateVolume = disks?.state_volume;
  // The disk that will physically hold the cache: the chosen one, else the state disk.
  const cacheVolume = disks?.disks.find((disk) => disk.volume_root === cacheDisk) ?? stateVolume;
  const leavesFree = cacheVolume ? Math.max(0, cacheVolume.free_bytes - floorBytes) : undefined;
  // Room MirageSSD may actually use locally right now: free space above the floor.
  const localRoom = cacheVolume ? cacheVolume.free_bytes - floorBytes : undefined;
  const floorBlocksWrites = localRoom !== undefined && localRoom <= 0;

  return (
    <section className="surface fine-grid min-h-96 rounded-[2rem] p-7 md:p-10" aria-labelledby="setup-title">
      <div className="max-w-xl pt-8 md:ml-[10%]">
        <Stepper step={step} />
        <span className="grid size-12 place-items-center rounded-2xl border border-white/10 bg-white/4 text-emerald-200">
          <HardDrives size={24} weight="duotone" aria-hidden="true" />
        </span>
        <h2 id="setup-title" className="mt-6 text-2xl font-semibold tracking-[-0.035em] text-zinc-50">
          {step === 'signin' && 'Connect your Google Drive.'}
          {step === 'create' && 'Create your drive.'}
          {step === 'done' && 'Your drive is ready.'}
        </h2>

        {step === 'signin' && <>
          <p className="mt-3 max-w-[58ch] text-sm leading-7 text-zinc-400">
            MirageSSD keeps a drive letter in Windows backed by your Google Drive. Sign in once — it reconnects automatically at every sign-in.
          </p>
          {(error || failed) && <ErrorCard message={error ?? failed ?? 'Sign-in failed.'} />}
          <button className="primary-button mt-6" onClick={() => void startLogin()} disabled={phase === 'in_flight'}>
            {phase === 'in_flight' ? <SpinnerGap size={16} className="refreshing" /> : <GoogleLogo size={16} weight="bold" />}
            {phase === 'in_flight' ? 'Waiting for Google…' : 'Sign in with Google'}
          </button>
          {phase === 'in_flight' && <p className="mt-3 text-xs text-zinc-500">Finish sign-in in the browser window that just opened, then come back here.</p>}
        </>}

        {step === 'create' && <>
          <p className="mt-3 max-w-[58ch] text-sm leading-7 text-zinc-400">
            Signed in{acct.email ? ` as ${acct.email}` : ''}. Choose a letter and how much local space MirageSSD may use for speed.{' '}
            <button
              className="quiet-button inline min-h-0 p-0 text-xs underline"
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
            <div className="mt-6 grid gap-4" aria-busy="true" aria-label="Loading disk options">
              <div className="skeleton-field" />
              <div className="skeleton-field" />
              <div className="skeleton-field" />
            </div>
          ) : (
          <div className="mt-6 grid gap-4">
            <label className="field"><span>Drive letter</span>
              <select aria-label="Drive letter" value={letter} onChange={(event) => setLetter(event.target.value)} disabled={creating}>
                {(disks?.free_letters?.length ? disks.free_letters : [letter || 'M']).map((free) => (
                  <option key={free} value={free}>{free}:</option>
                ))}
              </select>
            </label>
            <label className="field"><span>Name</span>
              <input value={name} maxLength={64} onChange={(event) => setName(event.target.value)} disabled={creating} />
            </label>
            <label className="field"><span>Keep the local cache on</span>
              <select aria-label="Cache disk" value={cacheDisk} onChange={(event) => {
                const next = event.target.value;
                setCacheDisk(next);
                const chosen = disks?.disks.find((disk) => disk.volume_root === next);
                if (chosen) { setFloorUnit('GiB'); setFloorValue(Math.max(1, Math.round(Math.max(chosen.total_bytes / 10, 20 * GIB) / GIB))); }
              }} disabled={creating}>
                {(disks?.disks ?? []).map((disk) => (
                  <option key={disk.volume_root} value={disk.volume_root}>{disk.volume_root} — {formatBytes(disk.free_bytes)} free of {formatBytes(disk.total_bytes)}</option>
                ))}
              </select>
              <small className="text-xs text-zinc-500">Files you use stay on this disk for speed; everything is also in your Google Drive.</small>
            </label>
            <label className="field"><span>Local SSD budget</span>
              <div className="flex items-center gap-3">
                <input type="number" min={1} max={4096} value={budgetValue} onChange={(event) => setBudgetValue(Math.max(1, Number(event.target.value)))} disabled={creating} aria-label="Local SSD budget" />
                <select aria-label="Budget unit" value={budgetUnit} onChange={(event) => setBudgetUnit(event.target.value as Unit)} disabled={creating}>
                  <option value="GiB">GiB</option>
                  <option value="MiB">MiB</option>
                </select>
              </div>
              {cacheVolume && <small className="text-xs text-zinc-500">On {cacheVolume.volume_root} — {formatBytes(cacheVolume.free_bytes)} free of {formatBytes(cacheVolume.total_bytes)}</small>}
            </label>
            <label className="field"><span>Always keep free</span>
              <div className="flex items-center gap-3">
                <input type="number" min={1} max={4096} value={floorValue} onChange={(event) => setFloorValue(Math.max(1, Number(event.target.value)))} disabled={creating} aria-label="Free-space floor" />
                <select aria-label="Floor unit" value={floorUnit} onChange={(event) => setFloorUnit(event.target.value as Unit)} disabled={creating}>
                  <option value="GiB">GiB</option>
                  <option value="MiB">MiB</option>
                </select>
              </div>
              <small className="text-xs text-zinc-500">
                MirageSSD evicts cloud-backed data before {cacheVolume?.volume_root ?? 'this disk'} fills past this point.
                {leavesFree !== undefined && !floorBlocksWrites && ` Right now that leaves ~${formatBytes(leavesFree)} for MirageSSD to use locally.`}
              </small>
              {floorBlocksWrites && cacheVolume && (
                <small className="text-xs text-amber-300" role="alert">
                  {cacheVolume.volume_root} only has {formatBytes(cacheVolume.free_bytes)} free, so with this floor MirageSSD can keep nothing on it and new files cannot be saved until you free space or lower the floor. Moving folders from {cacheVolume.volume_root} into this drive is how you get there.
                </small>
              )}
            </label>
          </div>
          )}
          {error && <ErrorCard message={error} />}
          <button className="primary-button mt-6" onClick={() => void startCreate()} disabled={creating || (disksLoading && !disks)}>
            {creating ? <SpinnerGap size={16} className="refreshing" /> : <CaretRight size={16} />}
            {creating ? (createStatus?.step ?? 'Working…') : 'Create drive'}
          </button>
        </>}

        {step === 'done' && <>
          {createStatus?.drive_letter && (
            <div className="drive-ready-card" role="status">
              <span className="drive-ready-letter">{createStatus.drive_letter}:</span>
              <span className="drive-ready-name">{createStatus.name ?? name}</span>
            </div>
          )}
          <p className="mt-3 max-w-[58ch] text-sm leading-7 text-zinc-400">
            Live in File Explorer. Everything you save syncs to your Google Drive automatically.
          </p>
          <div className="mt-6 flex flex-wrap items-center gap-3">
            {createStatus?.drive_letter && (
              <button className="primary-button" onClick={() => void client.openExplorer(createStatus.drive_letter!).catch(() => {})}>
                <FolderOpen size={16} />Open {createStatus.drive_letter}: in Explorer
              </button>
            )}
            {createStatus?.drive_letter && pinState !== 'done' && (
              <button className="secondary-button" onClick={() => void pinToQuickAccess(createStatus.drive_letter!)}>
                <PushPin size={16} />{pinState === 'failed' ? 'Try pinning again' : 'Pin to Quick Access'}
              </button>
            )}
            {pinState === 'done' && <span className="text-xs text-emerald-300" role="status">Pinned to Quick Access</span>}
            <button className="quiet-button" onClick={onDone}><CheckCircle size={16} />Done</button>
          </div>
        </>}
      </div>
    </section>
  );
}

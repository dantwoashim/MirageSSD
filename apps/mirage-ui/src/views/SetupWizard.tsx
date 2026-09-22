import { CaretRight, CheckCircle, FolderOpen, GoogleLogo, HardDrives, Info, SpinnerGap } from '@phosphor-icons/react';
import { useEffect, useRef, useState } from 'react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';
import type { ServiceClient } from '../api/client';
import type { DisksPayload, VolumeCreateStatus } from '../models';
import { formatBytes } from '../components/CapsuleBreakdown';

type Step = 'signin' | 'create' | 'done';

const GIB = 1 << 30;

export function SetupWizard({ client, account, onDone }: { client: ServiceClient; account: DriveAccount; onDone: () => void }) {
  const acct = useDriveAccount(account);
  const [step, setStep] = useState<Step>('signin');
  const [disks, setDisks] = useState<DisksPayload>();
  const [letter, setLetter] = useState('');
  const [name, setName] = useState('MirageSSD');
  const [budgetGiB, setBudgetGiB] = useState(0);
  const [floorGiB, setFloorGiB] = useState(0);
  const [createStatus, setCreateStatus] = useState<VolumeCreateStatus>();
  const [error, setError] = useState<string>();
  const polling = useRef(true);

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
    void client.disks().then((next) => {
      setDisks(next);
      setLetter(next.default_letter || 'M');
      setBudgetGiB(Math.max(1, Math.round(next.default_budget_bytes / GIB)));
      const state = next.state_volume;
      setFloorGiB(state ? Math.max(1, Math.round(Math.max(state.total_bytes / 10, 20 * GIB) / GIB)) : 20);
    }).catch((failure) => setError(failure instanceof Error ? failure.message : String(failure)));
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
        budget_bytes: budgetGiB * GIB,
        floor_bytes: floorGiB > 0 ? floorGiB * GIB : undefined,
      });
      setCreateStatus({ in_flight: true, step: 'starting' });
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    }
  };

  const phase = acct.phase === 'signing_in' ? 'in_flight' : acct.phase === 'error' ? 'failed' : 'idle';
  const failed = acct.phase === 'error' ? acct.error : undefined;
  const creating = Boolean(createStatus?.in_flight);

  return (
    <section className="surface fine-grid min-h-96 rounded-[2rem] p-7 md:p-10" aria-labelledby="setup-title">
      <div className="max-w-xl pt-8 md:ml-[10%]">
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
          {(error || failed) && <div className="feedback feedback-error" role="alert"><Info size={20} /><p>{error ?? failed}</p></div>}
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
            <label className="field"><span>Local SSD budget</span>
              <div className="flex items-center gap-3">
                <input type="range" min={1} max={256} value={budgetGiB} onChange={(event) => setBudgetGiB(Number(event.target.value))} disabled={creating} aria-label="Local SSD budget in GiB" />
                <span className="number text-sm text-zinc-300">{budgetGiB} GiB</span>
              </div>
              {disks?.state_volume && <small className="text-xs text-zinc-500">On {disks.state_volume.volume_root} — {formatBytes(disks.state_volume.free_bytes)} free of {formatBytes(disks.state_volume.total_bytes)}</small>}
            </label>
            <label className="field"><span>Always keep free</span>
              <div className="flex items-center gap-3">
                <input type="range" min={1} max={128} value={floorGiB} onChange={(event) => setFloorGiB(Number(event.target.value))} disabled={creating} aria-label="Free-space floor in GiB" />
                <span className="number text-sm text-zinc-300">{floorGiB} GiB</span>
              </div>
              <small className="text-xs text-zinc-500">MirageSSD evicts cloud-backed data before your disk fills past this point.</small>
            </label>
          </div>
          {error && <div className="feedback feedback-error" role="alert"><Info size={20} /><p>{error}</p></div>}
          <button className="primary-button mt-6" onClick={() => void startCreate()} disabled={creating}>
            {creating ? <SpinnerGap size={16} className="refreshing" /> : <CaretRight size={16} />}
            {creating ? (createStatus?.step ?? 'Working…') : 'Create drive'}
          </button>
        </>}

        {step === 'done' && <>
          <p className="mt-3 max-w-[58ch] text-sm leading-7 text-zinc-400">
            {createStatus?.drive_letter ? `${createStatus.drive_letter}: is` : 'Your drive is'} live in File Explorer. Everything you save syncs to your Google Drive automatically.
          </p>
          <div className="mt-6 flex items-center gap-3">
            {createStatus?.drive_letter && (
              <button className="primary-button" onClick={() => void client.openExplorer(createStatus.drive_letter!).catch(() => {})}>
                <FolderOpen size={16} />Open {createStatus.drive_letter}: in Explorer
              </button>
            )}
            <button className="quiet-button" onClick={onDone}><CheckCircle size={16} />Done</button>
          </div>
        </>}
      </div>
    </section>
  );
}

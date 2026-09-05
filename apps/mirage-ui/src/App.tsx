import {
  ArrowClockwise,
  ArrowsClockwise,
  Database,
  DownloadSimple,
  Gauge,
  HardDrives,
  House,
  Play,
  ShieldCheck,
  SignOut,
  Wrench,
} from '@phosphor-icons/react';
import { AnimatePresence, motion } from 'framer-motion';
import { memo, useCallback, useEffect, useMemo, useState } from 'react';
import type { ServiceClient } from './api/client';
import { EMPTY_READINESS, type CapacityLease, type CapacityPlan, type Mode, type Readiness, type ServiceSnapshot } from './models';
import { CapacityView } from './views/Capacity';
import { Dashboard } from './views/Dashboard';
import { ImportWizard } from './views/ImportWizard';
import { Launch } from './views/Launch';
import { Recovery } from './views/Recovery';
import { RepositoryDetail } from './views/RepositoryDetail';
import { UninstallPreparation } from './views/UninstallPreparation';
import { UpdateView } from './views/Update';

type View = 'overview' | 'capacity' | 'launch' | 'import' | 'update' | 'recovery' | 'exit';

const navigation = [
  { id: 'overview' as const, label: 'Overview', Icon: House },
  { id: 'capacity' as const, label: 'Capacity', Icon: Gauge },
  { id: 'launch' as const, label: 'Launch', Icon: Play },
  { id: 'import' as const, label: 'Convert', Icon: DownloadSimple },
  { id: 'update' as const, label: 'Update', Icon: ArrowsClockwise },
  { id: 'recovery' as const, label: 'Recovery', Icon: Wrench },
  { id: 'exit' as const, label: 'Restore and exit', Icon: SignOut },
];

export function App({ client }: { client: ServiceClient }) {
  const [view, setView] = useState<View>('overview');
  const [snapshot, setSnapshot] = useState<ServiceSnapshot | null>(null);
  const [selectedId, setSelectedId] = useState<string>();
  const [readiness, setReadiness] = useState<Readiness>(EMPTY_READINESS);
  const [mode, setMode] = useState<Mode>('verified_local');
  const [requestedGiB, setRequestedGiB] = useState(10);
  const [capacityPreview, setCapacityPreview] = useState<CapacityPlan>();
  const [capacityLease, setCapacityLease] = useState<CapacityLease>();
  const [nativeState, setNativeState] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [notice, setNotice] = useState<string>();

  const refresh = useCallback(async () => {
    try {
      const next = await client.snapshot();
      setSnapshot(next);
      setSelectedId((current) => current && next.repositories.some((item) => item.id === current)
        ? current
        : next.repositories[0]?.id);
      setError(undefined);
    } catch (caught) {
      setError(message(caught));
    }
  }, [client]);

  useEffect(() => {
    void refresh();
    const interval = window.setInterval(() => void refresh(), 5_000);
    return () => window.clearInterval(interval);
  }, [refresh]);

  const selected = useMemo(
    () => snapshot?.repositories.find((repository) => repository.id === selectedId),
    [selectedId, snapshot],
  );

  useEffect(() => {
    setCapacityPreview(undefined);
    setCapacityLease(undefined);
    setNativeState(undefined);
  }, [selectedId]);

  const perform = async <T,>(label: string, operation: () => Promise<T>): Promise<T | undefined> => {
    setBusy(true);
    setError(undefined);
    setNotice(undefined);
    try {
      const value = await operation();
      setNotice(`${label} completed${summary(value)}.`);
      await refresh();
      return value;
    } catch (caught) {
      setError(message(caught));
      return undefined;
    } finally {
      setBusy(false);
    }
  };

  const plan = async () => {
    if (!selected) return;
    const result = await perform('Capsule plan', () => client.plan(selected.id));
    if (isRecord(result)) {
      const capsuleId = stringValue(result.capsule_id);
      const totalBytes = numberValue(result.total_bytes);
      setReadiness({
        ...EMPTY_READINESS,
        state: stringValue(result.state) === 'sealed_ready' ? 'sealed_ready' : 'not_ready',
        capsuleId,
        hardSetBytes: numberValue(result.hard_set_bytes) || totalBytes,
        envelopeBytes: numberValue(result.envelope_bytes),
        scanMapBytes: numberValue(result.scan_map_bytes),
        frontierBytes: numberValue(result.frontier_bytes),
        updateReserveBytes: numberValue(result.update_reserve_bytes),
        missingBytes: numberValue(result.missing_bytes) || totalBytes,
        heldOutViolations: numberValue(result.held_out_violations),
      });
    }
  };

  const materialize = async () => {
    if (!selected || !readiness.capsuleId) return;
    const result = await perform('Capsule materialization', () => client.materialize(selected.id, readiness.capsuleId!));
    if (isRecord(result) && result.complete === true) {
      setReadiness((current) => ({ ...current, state: 'materializing', missingBytes: 0 }));
    }
  };

  const admit = async () => {
    if (!selected || !readiness.capsuleId || readiness.missingBytes !== 0) return;
    const result = await perform('Capsule admission', () => client.admit(selected.id, readiness.capsuleId!));
    if (isRecord(result) && stringValue(result.state) === 'sealed_ready') {
      setReadiness((current) => ({ ...current, state: 'sealed_ready', missingBytes: 0 }));
    }
  };

  const requestedBytes = requestedGiB * 1024 ** 3;

  const analyzeCapacity = async () => {
    if (!selected || !Number.isSafeInteger(requestedBytes) || requestedBytes <= 0) return;
    const result = await perform('Capacity analysis', async () => {
      const initial = await client.capacityPlan(selected.id, requestedBytes);
      return initial.origin === 'drive' && !initial.drive_authenticated
        ? await client.capacityPlan(selected.id, requestedBytes, true)
        : initial;
    });
    if (result) setCapacityPreview(result);
  };

  const acquireCapacity = async () => {
    if (!selected || capacityPreview?.requested_bytes !== requestedBytes) return;
    const result = await perform('Space Lease preparation', () => client.capacityAcquire(
      selected.id,
      requestedBytes,
      capacityPreview.origin === 'drive',
    ));
    if (result) setCapacityLease(result);
  };

  const refreshNative = async () => {
    if (!selected) return;
    const result = await perform('Native image status', () => client.nativeStatus(selected.id));
    if (isRecord(result)) setNativeState(stringValue(result.state));
  };

  const activateNative = async () => {
    if (!selected) return;
    const result = await perform('Native image activation', () => client.nativeActivate(selected.id));
    if (isRecord(result)) setNativeState(stringValue(result.state));
  };

  const releaseCapacity = async () => {
    if (!selected || !capacityLease) return;
    const result = await perform('Space Lease release', () => client.capacityRelease(selected.id, capacityLease.lease_id));
    if (result) {
      setCapacityLease(undefined);
      await analyzeCapacity();
    }
  };

  return (
    <div className="mx-auto grid min-h-[100dvh] max-w-[1600px] md:grid-cols-[16.5rem_minmax(0,1fr)]">
      <aside className="border-b border-white/8 bg-[#111513]/92 px-4 py-4 backdrop-blur-xl md:sticky md:top-0 md:h-[100dvh] md:border-r md:border-b-0 md:px-5 md:py-7">
        <div className="flex items-center justify-between md:block">
          <div className="flex items-center gap-3 px-2">
            <span className="grid size-9 place-items-center rounded-xl border border-emerald-300/20 bg-emerald-300/[0.07] text-emerald-200">
              <HardDrives size={19} weight="duotone" aria-hidden="true" />
            </span>
            <div>
              <p className="text-sm font-semibold tracking-[-0.02em] text-zinc-50">MirageSSD</p>
              <p className="number mt-0.5 text-[10px] text-zinc-600">CONTROL / V1</p>
            </div>
          </div>
          <div className="md:mt-10">
            <ServiceState connected={Boolean(snapshot) && !error} />
          </div>
        </div>
        <nav className="mt-4 flex gap-1 overflow-x-auto pb-1 md:mt-8 md:block md:space-y-1" aria-label="Management views">
          {navigation.map(({ id, label, Icon }) => (
            <button key={id} onClick={() => setView(id)} className={`relative flex shrink-0 items-center gap-3 rounded-xl px-3 py-2.5 text-sm transition duration-300 ease-out active:translate-y-px md:w-full ${view === id ? 'bg-white/[0.07] text-zinc-100' : 'text-zinc-500 hover:bg-white/[0.035] hover:text-zinc-300'}`}>
              {view === id && <motion.span layoutId="active-navigation" className="absolute inset-y-2 left-0 w-0.5 rounded-full bg-emerald-300" transition={{ type: 'spring', stiffness: 100, damping: 20 }} />}
              <Icon size={17} weight={view === id ? 'bold' : 'regular'} aria-hidden="true" />
              <span>{label}</span>
            </button>
          ))}
        </nav>
        <div className="mt-auto hidden border-t border-white/8 pt-5 text-xs leading-5 text-zinc-700 md:absolute md:right-5 md:bottom-7 md:left-5 md:block">
          Local control only.<br />No telemetry by default.
        </div>
      </aside>

      <main className="min-w-0 px-4 py-7 sm:px-6 md:px-9 md:py-9 lg:px-12">
        <header className="mb-8 flex flex-col justify-between gap-4 border-b border-white/8 pb-7 sm:flex-row sm:items-end">
          <div>
            <p className="text-xs font-semibold uppercase tracking-[0.2em] text-zinc-600">Windows asset tier</p>
            <h1 className="mt-2 text-3xl font-semibold tracking-[-0.045em] text-zinc-50">Keep the next read local.</h1>
          </div>
          {selected && <div className="flex items-center gap-2 text-xs text-zinc-500"><Database size={15} weight="bold" aria-hidden="true" /><span className="max-w-52 truncate">{selected.name}</span></div>}
        </header>

        <AnimatePresence mode="wait">
          {error && (
            <motion.div initial={{ opacity: 0, y: -6 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }} role="alert" className="mb-6 flex items-start justify-between gap-5 rounded-2xl border border-rose-300/20 bg-rose-300/[0.06] p-4 text-sm text-rose-100">
              <span>{error}</span>
              <button onClick={() => void refresh()} className="inline-flex shrink-0 items-center gap-1.5 text-xs font-semibold"><ArrowClockwise size={14} weight="bold" />Retry</button>
            </motion.div>
          )}
          {notice && !error && (
            <motion.div initial={{ opacity: 0, y: -6 }} animate={{ opacity: 1, y: 0 }} exit={{ opacity: 0 }} role="status" className="mb-6 flex items-center gap-2 rounded-2xl border border-emerald-300/20 bg-emerald-300/[0.055] p-4 text-sm text-emerald-100">
              <ShieldCheck size={17} weight="bold" aria-hidden="true" />{notice}
            </motion.div>
          )}
        </AnimatePresence>

        {!snapshot ? <LoadingState /> : (
          <motion.div key={view} initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} transition={{ type: 'spring', stiffness: 100, damping: 20 }}>
            {view === 'overview' && <div className="space-y-7"><Dashboard repositories={snapshot.repositories} selectedId={selectedId} onSelect={setSelectedId} onRefresh={() => void refresh()} />{selected && <><RepositoryDetail repository={selected} /><MountControls mounted={selected.mounted} canMount={selected.generation !== null} busy={busy} onMount={() => void perform('Mount', () => client.mount(selected.id, selected.generation!))} onUnmount={() => void perform('Unmount', () => client.unmount(selected.id))} /><NativeControls state={nativeState} busy={busy} onRefresh={() => void refreshNative()} onActivate={() => void activateNative()} /></>}</div>}
            {view === 'capacity' && selected && <CapacityView requestedGiB={requestedGiB} plan={capacityPreview} lease={capacityLease} busy={busy} onRequestedGiB={(value) => { setRequestedGiB(value); setCapacityPreview(undefined); }} onPlan={() => void analyzeCapacity()} onAcquire={() => void acquireCapacity()} onRelease={() => void releaseCapacity()} />}
            {view === 'launch' && selected && <Launch mode={mode} readiness={readiness} busy={busy} onMode={setMode} onPlan={() => void plan()} onMaterialize={() => void materialize()} onAdmit={() => void admit()} onLaunch={() => void perform('Launch', () => client.launch(selected.id, mode, readiness.state, readiness.capsuleId))} />}
            {view === 'import' && <ImportWizard />}
            {view === 'update' && selected && <UpdateView busy={busy} onBegin={() => void perform('Update start', () => client.beginUpdate(selected.id))} onRefresh={() => void perform('Update status', () => client.updateStatus(selected.id))} onCommit={() => void perform('Update commit', () => client.commitUpdate(selected.id))} onRollback={() => void perform('Update rollback', () => client.rollbackUpdate(selected.id))} />}
            {view === 'recovery' && selected && <Recovery busy={busy} onRepair={() => void perform('Repair', () => client.repair(selected.id))} />}
            {view === 'exit' && <UninstallPreparation mounted={selected?.mounted ?? false} />}
            {view !== 'overview' && view !== 'import' && view !== 'exit' && !selected && <NoSelection />}
          </motion.div>
        )}
      </main>
    </div>
  );
}

const ServiceState = memo(function ServiceState({ connected }: { connected: boolean }) {
  return (
    <div className="flex items-center gap-2 px-2 text-xs text-zinc-500" role="status">
      <motion.span animate={connected ? { opacity: [0.45, 1, 0.45], scale: [0.85, 1, 0.85] } : undefined} transition={{ duration: 2.4, repeat: Infinity, ease: 'easeInOut' }} className={`size-1.5 rounded-full ${connected ? 'bg-emerald-400' : 'bg-rose-300'}`} aria-hidden="true" />
      {connected ? 'Service connected' : 'Service unavailable'}
    </div>
  );
});

function MountControls({ mounted, canMount, busy, onMount, onUnmount }: { mounted: boolean; canMount: boolean; busy: boolean; onMount: () => void; onUnmount: () => void }) {
  return (
    <div className="flex flex-col justify-between gap-4 border-t border-white/8 pt-5 sm:flex-row sm:items-center">
      <p className="max-w-[60ch] text-xs leading-6 text-zinc-600">Mounting never virtualizes executables, DLLs, anti-cheat components, launchers, configuration, or mutable state.</p>
      <button onClick={mounted ? onUnmount : onMount} disabled={busy || (!mounted && !canMount)} className="shrink-0 rounded-xl border border-white/12 bg-white/5 px-4 py-2.5 text-sm font-medium text-zinc-200 transition duration-300 ease-out hover:bg-white/8 active:translate-y-px disabled:opacity-45">
        {busy ? 'Working' : mounted ? 'Unmount' : 'Mount active generation'}
      </button>
    </div>
  );
}

function NativeControls({ state, busy, onRefresh, onActivate }: { state?: string; busy: boolean; onRefresh: () => void; onActivate: () => void }) {
  return (
    <div className="flex flex-col justify-between gap-4 border-t border-white/8 pt-5 sm:flex-row sm:items-center">
      <div>
        <p className="text-sm font-medium text-zinc-200">Native Session Image</p>
        <p className="mt-1 text-xs leading-6 text-zinc-600">Materialize the verified Drive generation as an ordinary NTFS tree when maximum compatibility is required.</p>
        {state && <p className="number mt-1 text-xs text-emerald-200">{state.replaceAll('_', ' ')}</p>}
      </div>
      <div className="flex shrink-0 gap-2">
        <button onClick={onRefresh} disabled={busy} className="rounded-xl border border-white/12 bg-white/5 px-4 py-2.5 text-sm font-medium text-zinc-200 disabled:opacity-45">Check status</button>
        <button onClick={onActivate} disabled={busy} className="rounded-xl bg-emerald-500 px-4 py-2.5 text-sm font-semibold text-[#101713] disabled:bg-zinc-700 disabled:text-zinc-400">Activate native image</button>
      </div>
    </div>
  );
}

function LoadingState() {
  return (
    <div aria-label="Loading repository state" className="space-y-4">
      <div className="h-7 w-44 animate-pulse rounded-lg bg-white/6" />
      <div className="surface h-72 animate-pulse rounded-[2rem]" />
      <div className="grid gap-4 sm:grid-cols-[1.4fr_.6fr]"><div className="h-28 animate-pulse rounded-2xl bg-white/4" /><div className="h-28 animate-pulse rounded-2xl bg-white/4" /></div>
    </div>
  );
}

function NoSelection() {
  return <div className="surface rounded-[2rem] p-8 text-sm text-zinc-400">Select or configure a repository before using this view.</div>;
}

function message(caught: unknown): string {
  return caught instanceof Error ? caught.message : 'MirageSSD returned an unknown local error.';
}

function summary(value: unknown): string {
  if (!isRecord(value)) return '';
  const state = stringValue(value.state);
  return state ? ` with state ${state.replaceAll('_', ' ')}` : '';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function numberValue(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

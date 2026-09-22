import { ArrowClockwise, ArrowRight, CheckCircle, CloudSlash, Database, DownloadSimple, Gauge, HardDrives, House, Info, ShieldCheck, SignOut, Wrench, X } from '@phosphor-icons/react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createDriveAccount } from './api/account';
import type { ServiceClient } from './api/client';
import { AccountChip } from './components/AccountChip';
import { EMPTY_READINESS, type CapacityLease, type CapacityPlan, type Readiness, type RepositoryState, type ServiceSnapshot, type UpdateCheck } from './models';
import { operationMessage, readinessFromPlan, record, repositoryScope } from './presentation';
import { CapacityView } from './views/Capacity';
import { Dashboard } from './views/Dashboard';
import { DriveCard } from './views/DriveCard';
import { ImportWizard } from './views/ImportWizard';
import { SetupWizard } from './views/SetupWizard';
import { Launch } from './views/Launch';
import { Recovery } from './views/Recovery';
import { RepositoryDetail } from './views/RepositoryDetail';
import { UninstallPreparation } from './views/UninstallPreparation';
import { UpdateView } from './views/Update';

type View = 'overview' | 'capacity' | 'launch' | 'import' | 'setup' | 'update' | 'recovery' | 'exit';
type Scoped<T> = { scope: string; value: T };
const navigation = [
  { id: 'overview' as const, label: 'Your drives', Icon: House },
  { id: 'capacity' as const, label: 'Local storage', Icon: Gauge },
  { id: 'launch' as const, label: 'Offline access', Icon: DownloadSimple },
  { id: 'recovery' as const, label: 'Help & recovery', Icon: Wrench },
];
const titles: Record<View, [string, string]> = {
  overview: ['Your space, at a glance.', 'See what is on this device and what still needs your attention.'],
  capacity: ['Make room for what’s next.', 'Check available space before making a reservation.'],
  launch: ['Take your work with you.', 'Prepare and verify the files you need before going offline.'],
  recovery: ['Get back to your files.', 'Check a workspace and find a clear next step.'],
  import: ['Add a workspace.', 'Connect an existing imported workspace to MirageSSD.'],
  setup: ['Your own drive, ready in a minute.', 'Sign in once, pick a letter, and MirageSSD does the rest.'],
  update: ['Workspace versions.', 'Inspect an update before committing or rolling it back.'],
  exit: ['Keep your files with you.', 'Prepare a verified native copy before removing MirageSSD.'],
};

export function App({ client }: { client: ServiceClient }) {
  const [view, setView] = useState<View>('overview');
  const [snapshot, setSnapshot] = useState<ServiceSnapshot | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateCheck>();
  useEffect(() => {
    void client.updateCheck().then(setUpdateInfo).catch(() => {});
  }, [client]);
  const [selectedId, setSelectedId] = useState<string>();
  const selectedRef = useRef<string | undefined>(undefined);
  const [detail, setDetail] = useState<RepositoryState>();
  const [detailError, setDetailError] = useState<string>();
  const detailSequence = useRef(0);
  const [preparation, setPreparation] = useState<Scoped<Readiness>>();
  const [preview, setPreview] = useState<Scoped<CapacityPlan>>();
  const [lease, setLease] = useState<Scoped<CapacityLease>>();
  const [native, setNative] = useState<Scoped<string>>();
  const [requestedGiB, setRequestedGiB] = useState(10);
  const [busy, setBusy] = useState<string>();
  const operationLock = useRef(false);
  const [refreshing, setRefreshing] = useState(false);
  const refreshPromise = useRef<Promise<void> | null>(null);
  const [connectionError, setConnectionError] = useState<string>();
  const [actionError, setActionError] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [lastChecked, setLastChecked] = useState<Date>();
  const driveAccount = useMemo(() => createDriveAccount(client), [client]);

  const loadDetail = useCallback(async (id: string) => {
    const sequence = ++detailSequence.current;
    try {
      const next = await client.detail(id);
      if (sequence === detailSequence.current && selectedRef.current === id) {
        setDetail(next);
        setDetailError(undefined);
      }
    } catch (error) {
      if (sequence === detailSequence.current && selectedRef.current === id) {
        setDetail(undefined);
        setDetailError(message(error));
      }
    }
  }, [client]);

  const refresh = useCallback((): Promise<void> => {
    if (refreshPromise.current) return refreshPromise.current;
    setRefreshing(true);
    const request = (async () => {
      try {
        const next = await client.snapshot();
        setSnapshot(next);
        const id = next.repositories.some((item) => item.id === selectedRef.current)
          ? selectedRef.current : next.repositories[0]?.id;
        selectedRef.current = id;
        setSelectedId(id);
        setConnectionError(undefined);
        setLastChecked(new Date());
        if (id) await loadDetail(id);
        else setDetail(undefined);
      } catch (error) {
        setConnectionError(message(error));
      } finally {
        setRefreshing(false);
        refreshPromise.current = null;
      }
    })();
    refreshPromise.current = request;
    return request;
  }, [client, loadDetail]);

  useEffect(() => {
    let stopped = false;
    let timer: number | undefined;
    const poll = async () => {
      await refresh();
      if (!stopped) timer = window.setTimeout(() => void poll(), 5_000);
    };
    void poll();
    return () => { stopped = true; window.clearTimeout(timer); };
  }, [refresh]);

  const selected = useMemo(() => {
    const summary = snapshot?.repositories.find((item) => item.id === selectedId);
    return summary && detail?.id === summary.id && repositoryScope(detail) === repositoryScope(summary)
      ? { ...summary, ...detail } : summary;
  }, [snapshot, selectedId, detail]);
  const scope = repositoryScope(selected);
  const readiness = preparation?.scope === scope ? preparation.value : EMPTY_READINESS;
  const capacityPreview = preview?.scope === scope ? preview.value : undefined;
  const capacityLease = lease?.scope === scope ? lease.value : undefined;
  const nativeState = native?.scope === scope ? native.value : undefined;
  const unavailable = Boolean(connectionError) || !snapshot;
  const disabled = Boolean(busy) || unavailable;
  const requestedBytes = requestedGiB * 1024 ** 3;

  const select = (id: string) => {
    if (operationLock.current) return;
    selectedRef.current = id;
    setSelectedId(id);
    setDetail(undefined);
    setDetailError(undefined);
    setActionError(undefined);
    setNotice(undefined);
    void loadDetail(id);
  };

  const perform = async <T,>(label: string, operation: () => Promise<T>): Promise<T | undefined> => {
    if (operationLock.current || unavailable) return undefined;
    operationLock.current = true;
    setBusy(label);
    setActionError(undefined);
    setNotice(undefined);
    try {
      const value = await operation();
      setNotice(operationMessage(label, value));
      await refresh();
      return value;
    } catch (error) {
      setActionError(message(error));
      return undefined;
    } finally {
      operationLock.current = false;
      setBusy(undefined);
    }
  };

  const plan = async () => {
    if (!selected) return;
    const result = await perform('Offline check', async () => readinessFromPlan(await client.plan(selected.id)));
    if (result) setPreparation({ scope, value: result });
  };
  const materialize = async () => {
    if (!selected || !readiness.capsuleId) return;
    const result = await perform('File preparation', () => client.materialize(selected.id, readiness.capsuleId!));
    if (record(result) && result.complete === true) setPreparation({ scope, value: { ...readiness, state: 'materializing', missingBytes: 0 } });
  };
  const admit = async () => {
    if (!selected || !readiness.capsuleId || readiness.missingBytes !== 0) return;
    const result = await perform('Offline verification', () => client.admit(selected.id, readiness.capsuleId!));
    if (record(result) && result.state === 'sealed_ready') setPreparation({ scope, value: { ...readiness, state: 'sealed_ready', missingBytes: 0 } });
  };
  const analyzeCapacity = async () => {
    if (!selected || !Number.isSafeInteger(requestedBytes) || requestedBytes <= 0) return;
    const result = await perform('Storage check', async () => {
      const initial = await client.capacityPlan(selected.id, requestedBytes);
      return initial.origin === 'drive' && !initial.drive_authenticated ? client.capacityPlan(selected.id, requestedBytes, true) : initial;
    });
    if (result) setPreview({ scope, value: result });
  };
  const acquireCapacity = async () => {
    if (!selected || capacityPreview?.requested_bytes !== requestedBytes) return;
    const result = await perform('Space reservation', () => client.capacityAcquire(selected.id, requestedBytes, capacityPreview.origin === 'drive'));
    if (result) setLease({ scope, value: result });
  };
  const releaseCapacity = async () => {
    if (!selected || !capacityLease) return;
    const result = await perform('Reservation release', () => client.capacityRelease(selected.id, capacityLease.lease_id));
    if (result !== undefined) { setLease(undefined); setPreview(undefined); }
  };
  const nativeAction = async (activate: boolean) => {
    if (!selected) return;
    const result = await perform(activate ? 'Native copy preparation' : 'Native copy check', () => activate ? client.nativeActivate(selected.id) : client.nativeStatus(selected.id));
    if (record(result) && typeof result.state === 'string') setNative({ scope, value: result.state });
  };
  const [title, description] = titles[view];

  return (
    <div className="app-shell">
      <a className="skip-link" href="#main-content">Skip to content</a>
      <aside className="app-sidebar">
        <a className="brand" href="#" onClick={(event) => { event.preventDefault(); setView('overview'); }} aria-label="MirageSSD home">
          <span className="brand-mark"><HardDrives size={23} weight="duotone" /></span>
          <span>Mirage<span className="brand-suffix">SSD</span><small>A place for your files.</small></span>
        </a>
        <p className="nav-caption">Workspace</p>
        <nav aria-label="Main navigation" className="primary-nav">
          {navigation.map(({ id, label, Icon }) => <button key={id} onClick={() => setView(id)} aria-current={view === id ? 'page' : undefined} className={view === id ? 'nav-item is-active' : 'nav-item'}><Icon size={19} weight={view === id ? 'fill' : 'regular'} aria-hidden="true" />{label}</button>)}
        </nav>
        <details className="advanced-nav">
          <summary>Workspace tools</summary>
          <button className="nav-item" aria-current={view === 'import' ? 'page' : undefined} onClick={() => setView('import')}><Database size={18} />Add workspace</button>
          <button className="nav-item" aria-current={view === 'update' ? 'page' : undefined} onClick={() => setView('update')}><ArrowClockwise size={18} />Versions & updates</button>
          <button className="nav-item" aria-current={view === 'exit' ? 'page' : undefined} onClick={() => setView('exit')}><SignOut size={18} />Restore & leave</button>
        </details>
        <div className="sidebar-note"><ShieldCheck size={19} aria-hidden="true" /><div>Your storage. Your control.<p>Keep files close. Keep your options open.</p></div></div>
        <AccountChip account={driveAccount} onChanged={() => void refresh()} />
        <div className="service-indicator" role="status"><span className={unavailable ? 'status-dot status-muted' : 'status-dot'} />{snapshot ? connectionError ? 'Connection interrupted' : 'Desktop service connected' : connectionError ? 'Desktop service unavailable' : 'Connecting to your desktop…'}</div>
        <button className="quiet-button diagnostics-link" onClick={() => {
          client.diagnosticsCollect()
            .then((path) => setNotice(`Diagnostics saved to ${path}`))
            .catch((failure: unknown) => setActionError(failure instanceof Error ? failure.message : String(failure)));
        }}>Collect diagnostics</button>
      </aside>

      <main id="main-content" className="app-main" tabIndex={-1}>
        <div className="workspace-bar"><span>Personal workspace <span className="workspace-separator">/</span> {navigation.find((item) => item.id === view)?.label ?? 'Tools'}</span><button className="quiet-button" onClick={() => void refresh()} disabled={refreshing} aria-label="Refresh drive status"><ArrowClockwise size={16} className={refreshing ? 'refreshing' : ''} />{refreshing ? 'Checking' : 'Refresh'}</button></div>
        <header className="page-heading"><div><span className="eyebrow">MirageSSD workspace</span><h1>{title}</h1><p>{description}</p></div>{selected && <label className="drive-selector"><span>Selected drive</span><select aria-label="Selected drive" value={selectedId} onChange={(event) => select(event.target.value)} disabled={Boolean(busy)}>{snapshot?.repositories.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>}</header>

        {updateInfo?.update_available && updateInfo.url && (
          <a className="update-banner" href={updateInfo.url} target="_blank" rel="noreferrer">
            <DownloadSimple size={14} />{(updateInfo.latest ?? '').replace(/^v/, '')} is available — Download
          </a>
        )}
        {connectionError && <div className="feedback feedback-warning" role="alert"><CloudSlash size={22} /><div><strong>{snapshot ? 'Showing the last known state' : 'Let’s reconnect your desktop'}</strong><p>{connectionError}</p>{lastChecked && <small>Last connected at {lastChecked.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}. Actions are paused until the connection returns.</small>}</div><button className="quiet-button" onClick={() => void refresh()} disabled={refreshing}>Reconnect</button></div>}
        {actionError && <div className="feedback feedback-error" role="alert"><Info size={22} /><div><strong>The operation needs your attention</strong><p>{actionError}</p></div><button className="icon-button" aria-label="Dismiss operation error" onClick={() => setActionError(undefined)}><X size={18} /></button></div>}
        {notice && !actionError && <div className="feedback feedback-success" role="status"><CheckCircle size={21} /><p>{notice}</p><button className="icon-button" aria-label="Dismiss notification" onClick={() => setNotice(undefined)}><X size={18} /></button></div>}
        {busy && <div className="working-banner" role="status"><span className="working-indicator" /><span>{busy} in progress. You can keep this window open while MirageSSD works.</span></div>}

        {!snapshot ? connectionError ? <section className="disconnected-state"><HardDrives size={40} weight="duotone" /><h2>Your files haven’t been changed.</h2><p>Open MirageSSD from the Windows Start menu. When its desktop service is ready, this window will reconnect automatically.</p><button className="primary-button" onClick={() => void refresh()} disabled={refreshing}>Try reconnecting <ArrowRight size={17} /></button></section> : <LoadingState /> : <div className="view-content">
          {view === 'setup' && <SetupWizard client={client} account={driveAccount} onDone={() => { setView('overview'); void refresh(); }} />}
          {view === 'overview' && <>
            <Dashboard repositories={snapshot.repositories} selectedId={selectedId} onSelect={select} onRefresh={() => void refresh()} busy={Boolean(busy)} onSetup={() => setView('setup')} />
            {selected && selected.origin === 'drive' && selected.volumeMode === 'managed' && (
              <DriveCard
                client={client}
                repository={selected}
                busy={disabled}
                onToggleMount={() => void perform(selected.mounted ? 'Drive disconnect' : 'Drive connection', () => selected.mounted ? client.unmount(selected.id) : client.mount(selected.id, selected.generation ?? 0))}
                onChanged={(note, error) => { if (error) setActionError(error); else if (note) setNotice(note); void refresh(); }}
              />
            )}
            {selected && <>
              {detailError && <div className="feedback feedback-warning" role="status"><Info size={20} /><p>Detailed storage status is unavailable. {detailError}</p></div>}
              <RepositoryDetail repository={selected} />
              <section className="drive-actions"><div><h3>{selected.mounted ? 'Ready in your file manager' : 'Open this workspace'}</h3><p>{selected.mounted ? 'Close files using this drive before disconnecting it.' : 'Connect the current verified version to make its files available.'}</p></div><button className={selected.mounted ? 'secondary-button' : 'primary-button'} disabled={disabled || (!selected.mounted && selected.generation === null)} onClick={() => void perform(selected.mounted ? 'Drive disconnect' : 'Drive connection', () => selected.mounted ? client.unmount(selected.id) : client.mount(selected.id, selected.generation!))}>{selected.mounted ? 'Disconnect drive' : 'Connect drive'}<ArrowRight size={16} /></button></section>
              <details className="advanced-panel"><summary>Application compatibility</summary><p>Prepare a verified copy on ordinary local storage for applications that need it. This requires additional disk space.</p>{nativeState && <p role="status">Native copy: {nativeState.replaceAll('_', ' ')}</p>}<div className="button-row"><button className="secondary-button" disabled={disabled} onClick={() => void nativeAction(false)}>Check native copy</button><button className="secondary-button" disabled={disabled} onClick={() => void nativeAction(true)}>Prepare native copy</button></div></details>
            </>}
          </>}
          {view === 'capacity' && selected && <CapacityView requestedGiB={requestedGiB} plan={capacityPreview} lease={capacityLease} busy={disabled} onRequestedGiB={(value) => { setRequestedGiB(value); setPreview(undefined); }} onPlan={() => void analyzeCapacity()} onAcquire={() => void acquireCapacity()} onRelease={() => void releaseCapacity()} />}
          {view === 'launch' && selected && <Launch mode="verified_local" readiness={readiness} busy={disabled} onMode={() => {}} onPlan={() => void plan()} onMaterialize={() => void materialize()} onAdmit={() => void admit()} onLaunch={() => void perform('Application launch', () => client.launch(selected.id, 'verified_local', readiness.state, readiness.capsuleId))} />}
          {view === 'import' && <ImportWizard />}
          {view === 'update' && selected && <UpdateView client={client} busy={disabled} onBegin={() => void perform('Update preparation', () => client.beginUpdate(selected.id))} onRefresh={() => void perform('Update check', () => client.updateStatus(selected.id))} onCommit={() => void perform('Version activation', () => client.commitUpdate(selected.id))} onRollback={() => void perform('Version rollback', () => client.rollbackUpdate(selected.id))} />}
          {view === 'recovery' && selected && <Recovery busy={disabled} onRepair={() => void perform('Workspace check', () => client.repair(selected.id))} />}
          {view === 'exit' && <UninstallPreparation mounted={selected?.mounted ?? false} />}
          {!selected && !['overview', 'import', 'exit'].includes(view) && <section className="empty-state"><Database size={30} /><h2>Choose a workspace first</h2><p>Your storage and recovery tools appear here when a workspace is connected.</p><button className="primary-button" onClick={() => setView('import')}>Add a workspace <ArrowRight size={17} /></button></section>}
        </div>}
        <footer className="workspace-footer"><span>Files on your terms.</span><span>{lastChecked ? `Status checked at ${lastChecked.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}` : 'Waiting for the desktop service'}</span></footer>
      </main>
    </div>
  );
}

function LoadingState() {
  return <section className="loading-state" aria-label="Loading your drives" role="status"><span className="skeleton skeleton-title" /><span className="skeleton skeleton-panel" /><span className="skeleton skeleton-row" /><span className="sr-only">Connecting to the desktop service…</span></section>;
}
function message(value: unknown) { return value instanceof Error ? value.message : 'MirageSSD could not complete this operation. Refresh its status before retrying.'; }

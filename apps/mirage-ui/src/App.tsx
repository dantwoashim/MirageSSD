import { ArrowClockwise, Database, DownloadSimple, HardDrives } from '@phosphor-icons/react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { createDriveAccount } from './api/account';
import type { ServiceClient } from './api/client';
import { Sidebar } from './components/Sidebar';
import { formatClock } from './format';
import { message, useServiceState } from './hooks/useServiceState';
import { EMPTY_READINESS, type CapacityLease, type CapacityPlan, type Readiness, type UpdateCheck } from './models';
import { isManagedDrive, operationMessage, readinessFromPlan, record, repositoryScope } from './presentation';
import { Button, ButtonLink, Card, EmptyState, Notice, PageHeader, Select, Skeleton, Spinner, useToast } from './ui';
import { CapacityView } from './views/Capacity';
import { Dashboard } from './views/Dashboard';
import { DrivePanel } from './views/drive/DrivePanel';
import { HelpView } from './views/Help';
import { ImportWizard } from './views/ImportWizard';
import { SetupWizard } from './views/SetupWizard';
import { Launch } from './views/Launch';
import { Recovery } from './views/Recovery';
import { RepositoryDetail } from './views/RepositoryDetail';
import { SettingsView } from './views/Settings';
import { UninstallPreparation } from './views/UninstallPreparation';
import { UpdateView } from './views/Update';

type View = 'overview' | 'capacity' | 'launch' | 'import' | 'setup' | 'update' | 'recovery' | 'exit' | 'settings' | 'help';
type Scoped<T> = { scope: string; value: T };

const ADVANCED_VIEWS: View[] = ['capacity', 'launch', 'update', 'recovery', 'import', 'exit'];

export function App({ client }: { client: ServiceClient }) {
  const [view, setView] = useState<View>(() => {
    const requested = new URLSearchParams(window.location.search).get('view');
    const allowed: View[] = ['overview', 'capacity', 'launch', 'import', 'setup', 'update', 'recovery', 'exit', 'settings', 'help'];
    return allowed.includes(requested as View) ? (requested as View) : 'overview';
  });
  const [updateInfo, setUpdateInfo] = useState<UpdateCheck>();
  useEffect(() => {
    void client.updateCheck().then(setUpdateInfo).catch(() => {});
  }, [client]);
  const {
    snapshot,
    selectedId,
    selected,
    detailError,
    select,
    refresh,
    refreshing,
    connectionError,
    lastChecked,
    operationLock,
  } = useServiceState(client);
  const [preparation, setPreparation] = useState<Scoped<Readiness>>();
  const [preview, setPreview] = useState<Scoped<CapacityPlan>>();
  const [lease, setLease] = useState<Scoped<CapacityLease>>();
  const [native, setNative] = useState<Scoped<string>>();
  const [requestedGiB, setRequestedGiB] = useState(10);
  const [busy, setBusy] = useState<string>();
  const driveAccount = useMemo(() => createDriveAccount(client), [client]);
  const toast = useToast();
  // First run: with no drives yet the only useful thing to do is set one up,
  // so the app opens the setup flow instead of an overview with one button.
  const firstRunRouted = useRef(false);
  useEffect(() => {
    if (!snapshot || firstRunRouted.current) return;
    firstRunRouted.current = true;
    if (snapshot.repositories.length === 0 && view === 'overview') setView('setup');
  }, [snapshot, view]);

  const titles: Record<View, [string, string | undefined]> = {
    overview: ['Drives', snapshot?.repositories.length ? 'See what is on this PC and what is still uploading.' : undefined],
    settings: ['Settings', 'Account, appearance, updates, and diagnostics.'],
    help: ['Help', 'Fix common problems and find support.'],
    setup: [snapshot && snapshot.repositories.length > 0 ? 'Add a drive' : 'Set up MirageSSD', undefined],
    capacity: ['Local storage', 'Check available space before making a reservation.'],
    launch: ['Offline access', 'Prepare and verify the files you need before going offline.'],
    update: ['Versions', 'Inspect an update before committing or rolling it back.'],
    recovery: ['Recovery', 'Check a workspace and find a clear next step.'],
    import: ['Add a workspace', 'Connect an existing imported workspace to MirageSSD.'],
    exit: ['Restore before uninstalling', 'Prepare a verified native copy before removing MirageSSD.'],
  };
  const [title, description] = titles[view];
  useEffect(() => {
    document.title = `${title} — MirageSSD`;
  }, [title]);

  const scope = repositoryScope(selected);
  const readiness = preparation?.scope === scope ? preparation.value : EMPTY_READINESS;
  const capacityPreview = preview?.scope === scope ? preview.value : undefined;
  const capacityLease = lease?.scope === scope ? lease.value : undefined;
  const nativeState = native?.scope === scope ? native.value : undefined;
  const unavailable = Boolean(connectionError) || !snapshot;
  const disabled = Boolean(busy) || unavailable;
  const requestedBytes = requestedGiB * 1024 ** 3;
  const showAdvanced = Boolean(snapshot?.repositories.some((item) => !isManagedDrive(item))) || ADVANCED_VIEWS.includes(view);
  const serviceState = snapshot
    ? connectionError ? 'reconnecting' : 'running'
    : connectionError ? 'unavailable' : 'reconnecting';

  const perform = async <T,>(label: string, operation: () => Promise<T>): Promise<T | undefined> => {
    if (operationLock.current || unavailable) return undefined;
    operationLock.current = true;
    setBusy(label);
    try {
      const value = await operation();
      toast.success(operationMessage(label, value));
      await refresh();
      return value;
    } catch (error) {
      toast.error(message(error));
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
  const toggleMount = () => {
    if (!selected) return;
    void perform(selected.mounted ? 'Drive disconnect' : 'Drive connection', () =>
      selected.mounted ? client.unmount(selected.id) : client.mount(selected.id, selected.generation ?? 0));
  };
  const onDriveChanged = (note?: string, error?: string) => {
    if (error) toast.error(error);
    else if (note) toast.success(note);
    void refresh();
  };

  return (
    <div className="grid min-h-dvh md:grid-cols-[240px_minmax(0,1fr)]">
      <a className="skip-link" href="#main-content">Skip to content</a>
      <Sidebar
        view={view}
        onNavigate={(next) => setView(next as View)}
        showAdvanced={showAdvanced}
        account={driveAccount}
        accountUnavailable={!snapshot && connectionError !== undefined}
        service={serviceState}
      />

      <main id="main-content" tabIndex={-1} className="min-w-0 px-6 py-6 outline-none md:px-10 md:py-8">
        <div className="mx-auto grid w-full max-w-[1040px] gap-6">
          <PageHeader
            title={title}
            description={description}
            actions={
              <>
                {lastChecked && <span className="whitespace-nowrap text-xs text-fg-subtle">Updated {formatClock(lastChecked)}</span>}
                <Button variant="ghost" size="sm" icon={<ArrowClockwise size={15} className={refreshing ? 'animate-spin' : undefined} />} onClick={() => void refresh()} disabled={refreshing} aria-label="Refresh drive status">
                  Refresh
                </Button>
                {ADVANCED_VIEWS.includes(view) && snapshot && snapshot.repositories.length > 1 && (
                  <Select aria-label="Selected workspace" value={selectedId} onChange={(event) => select(event.target.value)} disabled={Boolean(busy)} className="w-auto min-w-40">
                    {snapshot.repositories.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
                  </Select>
                )}
                {view === 'setup' && snapshot && snapshot.repositories.length > 0 && (
                  <Button variant="ghost" size="sm" onClick={() => setView('overview')}>Back to drives</Button>
                )}
              </>
            }
          />

          {updateInfo?.update_available && updateInfo.url && (
            <Notice
              tone="info"
              title={`MirageSSD ${(updateInfo.latest ?? '').replace(/^v/, '')} is available.`}
              action={<ButtonLink variant="secondary" size="sm" href={updateInfo.url} icon={<DownloadSimple size={14} />}>Download</ButtonLink>}
            />
          )}
          {connectionError && snapshot && (
            <Notice
              tone="warning"
              title="Showing the last known state"
              action={<Button variant="secondary" size="sm" onClick={() => void refresh()} disabled={refreshing}>Reconnect</Button>}
            >
              <p>{connectionError}</p>
              {lastChecked && <p className="mt-0.5">Last connected at {formatClock(lastChecked)}. Actions are paused until the connection returns.</p>}
            </Notice>
          )}
          {busy && (
            <div role="status" className="flex items-center gap-2 text-[13px] text-fg-muted">
              <Spinner size={15} />
              <span>{busy}…</span>
            </div>
          )}

          {!snapshot ? connectionError ? (
            <DisconnectedState
              bridgeTokenLost={connectionError.includes('bridge token')}
              busy={refreshing}
              onStartService={() => void client.serviceStart().then(() => window.setTimeout(() => void refresh(), 3000)).catch((failure) => toast.error(message(failure)))}
              onDiagnostics={() => void client.diagnosticsCollect().then((path) => toast.success(`Diagnostics saved to ${path}`)).catch((failure) => toast.error(message(failure)))}
              onReconnect={() => void refresh()}
            />
          ) : (
            <LoadingState />
          ) : (
            <div key={view} className="view-in grid gap-4">
                {view === 'setup' && <SetupWizard client={client} account={driveAccount} autoCreate={snapshot.repositories.length === 0} onDone={() => { setView('overview'); void refresh(); }} />}
                {view === 'settings' && <SettingsView client={client} account={driveAccount} updateInfo={updateInfo} unavailable={Boolean(connectionError)} onChanged={() => void refresh()} />}
                {view === 'help' && <HelpView client={client} serviceDown={Boolean(connectionError)} onRefresh={() => void refresh()} />}
                {view === 'overview' && <>
                  <Dashboard repositories={snapshot.repositories} selectedId={selectedId} onSelect={select} busy={Boolean(busy)} onSetup={() => setView('setup')} />
                  {selected && detailError && <Notice tone="warning">Detailed storage status is unavailable. {detailError}</Notice>}
                  {selected && isManagedDrive(selected) && (
                    <DrivePanel
                      client={client}
                      repository={selected}
                      busy={disabled}
                      onToggleMount={toggleMount}
                      onChanged={onDriveChanged}
                    />
                  )}
                  {selected && !isManagedDrive(selected) && <>
                    <RepositoryDetail repository={selected} />
                    <Card>
                      <div className="flex flex-wrap items-center justify-between gap-4">
                        <div className="min-w-0">
                          <h3 className="section-title text-[15px]">{selected.mounted ? 'Ready in your file manager' : 'Open this workspace'}</h3>
                          <p className="mt-1 max-w-[58ch] text-xs leading-relaxed text-fg-muted">{selected.mounted ? 'Close files using this drive before disconnecting it.' : 'Connect the current verified version to make its files available.'}</p>
                        </div>
                        <Button
                          variant={selected.mounted ? 'secondary' : 'primary'}
                          disabled={disabled || (!selected.mounted && selected.generation === null)}
                          onClick={toggleMount}
                        >
                          {selected.mounted ? 'Disconnect drive' : 'Connect drive'}
                        </Button>
                      </div>
                    </Card>
                    <Card>
                      <details>
                        <summary className="cursor-pointer text-[13px] font-medium text-fg-muted transition-colors duration-150 ease-standard hover:text-fg">Application compatibility</summary>
                        <p className="mt-3 max-w-[65ch] text-[13px] leading-relaxed text-fg-muted">Prepare a verified copy on ordinary local storage for applications that need it. This requires additional disk space.</p>
                        {nativeState && <p className="mt-3 text-[13px] text-fg" role="status">Native copy: {nativeState.replaceAll('_', ' ')}</p>}
                        <div className="mt-4 flex flex-wrap gap-2">
                          <Button variant="secondary" size="sm" disabled={disabled} onClick={() => void nativeAction(false)}>Check native copy</Button>
                          <Button variant="secondary" size="sm" disabled={disabled} onClick={() => void nativeAction(true)}>Prepare native copy</Button>
                        </div>
                      </details>
                    </Card>
                  </>}
                </>}
                {view === 'capacity' && selected && <CapacityView requestedGiB={requestedGiB} plan={capacityPreview} lease={capacityLease} busy={disabled} onRequestedGiB={(value) => { setRequestedGiB(value); setPreview(undefined); }} onPlan={() => void analyzeCapacity()} onAcquire={() => void acquireCapacity()} onRelease={() => void releaseCapacity()} />}
                {view === 'launch' && selected && <Launch mode="verified_local" readiness={readiness} busy={disabled} onPlan={() => void plan()} onMaterialize={() => void materialize()} onAdmit={() => void admit()} onLaunch={() => void perform('Application launch', () => client.launch(selected.id, 'verified_local', readiness.state, readiness.capsuleId))} />}
                {view === 'import' && <ImportWizard />}
                {view === 'update' && selected && <UpdateView busy={disabled} onBegin={() => void perform('Update preparation', () => client.beginUpdate(selected.id))} onRefresh={() => void perform('Update check', () => client.updateStatus(selected.id))} onCommit={() => void perform('Version activation', () => client.commitUpdate(selected.id))} onRollback={() => void perform('Version rollback', () => client.rollbackUpdate(selected.id))} />}
                {view === 'recovery' && selected && <Recovery busy={disabled} onRepair={() => void perform('Workspace check', () => client.repair(selected.id))} />}
                {view === 'exit' && <UninstallPreparation mounted={selected?.mounted ?? false} />}
                {!selected && !['overview', 'import', 'exit', 'setup', 'settings', 'help'].includes(view) && (
                  <EmptyState
                    icon={<Database size={24} weight="duotone" />}
                    title="Choose a workspace first"
                    body="Your storage and recovery tools appear here when a workspace is connected."
                    actions={<Button onClick={() => setView('import')}>Add a workspace</Button>}
                  />
                )}
            </div>
          )}
        </div>
      </main>
    </div>
  );
}

function DisconnectedState({
  bridgeTokenLost,
  busy,
  onStartService,
  onDiagnostics,
  onReconnect,
}: {
  bridgeTokenLost: boolean;
  busy: boolean;
  onStartService: () => void;
  onDiagnostics: () => void;
  onReconnect: () => void;
}) {
  if (bridgeTokenLost) {
    return (
      <EmptyState
        icon={<HardDrives size={24} weight="duotone" />}
        title="This window lost its connection to the MirageSSD app"
        body="Close it and open MirageSSD again from the Start menu."
      />
    );
  }
  return (
    <EmptyState
      icon={<HardDrives size={24} weight="duotone" />}
      title="MirageSSD's background service isn't running"
      body="Your files on Drive are safe. Start the service to bring your drives back — Windows may ask for permission."
      actions={
        <>
          <Button disabled={busy} onClick={onStartService}>Start service</Button>
          <Button variant="secondary" onClick={onDiagnostics}>Collect diagnostics</Button>
          <Button variant="ghost" disabled={busy} onClick={onReconnect}>Try reconnecting</Button>
        </>
      }
    />
  );
}

function LoadingState() {
  return (
    <section className="grid gap-3" aria-label="Loading your drives" role="status">
      <div className="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-3">
        <Skeleton className="h-[116px] rounded-xl" />
        <Skeleton className="h-[116px] rounded-xl" />
        <Skeleton className="h-[116px] rounded-xl" />
      </div>
      <Skeleton className="h-72 rounded-2xl" />
      <span className="sr-only">Connecting to the desktop service…</span>
    </section>
  );
}

import {
  ArrowClockwise,
  Database,
  DownloadSimple,
  Gauge,
  GearSix,
  GoogleLogo,
  HardDrives,
  Question,
  SignOut,
  Wrench,
} from '@phosphor-icons/react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';
import { cx } from '../ui';

export type ServiceState = 'running' | 'reconnecting' | 'unavailable';

const primary = [
  { id: 'overview', label: 'Drives', Icon: HardDrives },
  { id: 'settings', label: 'Settings', Icon: GearSix },
  { id: 'help', label: 'Help', Icon: Question },
] as const;

const advanced = [
  { id: 'capacity', label: 'Local storage', Icon: Gauge },
  { id: 'launch', label: 'Offline access', Icon: DownloadSimple },
  { id: 'update', label: 'Versions', Icon: ArrowClockwise },
  { id: 'recovery', label: 'Recovery', Icon: Wrench },
  { id: 'import', label: 'Add a workspace', Icon: Database },
  { id: 'exit', label: 'Restore before uninstall', Icon: SignOut },
] as const;

export function Sidebar({
  view,
  onNavigate,
  showAdvanced,
  account,
  accountUnavailable,
  service,
}: {
  view: string;
  onNavigate: (view: string) => void;
  showAdvanced: boolean;
  account: DriveAccount;
  accountUnavailable?: boolean;
  service: ServiceState;
}) {
  const isActive = (id: string) => view === id || (id === 'overview' && view === 'setup');
  const item = ({ id, label, Icon }: { id: string; label: string; Icon: typeof HardDrives }) => (
    <button
      key={id}
      type="button"
      onClick={() => onNavigate(id)}
      aria-current={isActive(id) ? 'page' : undefined}
      className={cx(
        'flex shrink-0 items-center gap-2.5 rounded-lg px-3 py-2 text-left text-[13px] transition-colors duration-150 ease-standard max-[720px]:py-1.5',
        isActive(id) ? 'bg-accent-soft font-medium text-accent' : 'text-fg-muted hover:bg-surface-2 hover:text-fg',
      )}
    >
      <Icon size={17} weight={isActive(id) ? 'fill' : 'regular'} aria-hidden="true" />
      {label}
    </button>
  );

  return (
    <aside className="border-line bg-surface max-[720px]:border-b md:border-r">
      <div className="flex h-full flex-col md:sticky md:top-0 md:h-dvh">
        <div className="flex items-center gap-2.5 px-5 pb-4 pt-5 max-[720px]:pb-2 max-[720px]:pt-3">
          <a
            href="#"
            onClick={(event) => {
              event.preventDefault();
              onNavigate('overview');
            }}
            aria-label="MirageSSD home"
            className="flex items-center gap-2.5 font-display text-[17px] font-semibold tracking-[-0.01em] text-fg no-underline"
          >
            <span className="grid size-8 place-items-center rounded-lg border border-line bg-accent-soft text-accent" aria-hidden="true">
              <HardDrives size={18} weight="duotone" />
            </span>
            MirageSSD
          </a>
        </div>
        <nav aria-label="Main navigation" className="grid gap-0.5 overflow-x-auto px-3 max-[720px]:flex max-[720px]:px-4 max-[720px]:pb-2">
          {primary.map(item)}
        </nav>
        {showAdvanced && (
          <nav aria-label="Advanced" className="mt-5 grid gap-0.5 px-3 max-[720px]:mt-0 max-[720px]:flex max-[720px]:overflow-x-auto max-[720px]:px-4 max-[720px]:pb-2">
            <p className="field-label px-3 pb-1 max-[720px]:hidden">Advanced</p>
            {advanced.map(item)}
          </nav>
        )}
        <div className="mt-auto grid gap-1 border-t border-line px-3 py-3 max-[720px]:hidden">
          <AccountSummary account={account} unavailable={accountUnavailable} onOpenSettings={() => onNavigate('settings')} />
          {service === 'running' ? (
            <p role="status" className="flex items-center gap-2 px-2.5 py-1 text-xs text-fg-subtle">
              <span className="status-dot text-ok" aria-hidden="true" />
              Service running
            </p>
          ) : (
            <button
              type="button"
              onClick={() => onNavigate('help')}
              className="flex items-center gap-2 rounded-lg px-2.5 py-1 text-left text-xs text-fg-muted transition-colors duration-150 ease-standard hover:bg-surface-2 hover:text-fg"
            >
              <span className={service === 'reconnecting' ? 'busy-dot text-warn' : 'status-dot text-danger'} aria-hidden="true" />
              {service === 'reconnecting' ? 'Reconnecting…' : 'Service unavailable'}
            </button>
          )}
        </div>
      </div>
    </aside>
  );
}

export function AccountSummary({
  account,
  unavailable,
  onOpenSettings,
}: {
  account: DriveAccount;
  unavailable?: boolean;
  onOpenSettings: () => void;
}) {
  if (unavailable) {
    return (
      <div className="flex items-center gap-2.5 px-2.5 py-1.5 text-fg-subtle" aria-live="polite">
        <span className="grid size-8 shrink-0 place-items-center rounded-lg border border-line" aria-hidden="true">
          <GoogleLogo size={15} weight="bold" />
        </span>
        <span className="text-xs">Unavailable</span>
      </div>
    );
  }
  return <AccountSummaryInner account={account} onOpenSettings={onOpenSettings} />;
}

function AccountSummaryInner({ account, onOpenSettings }: { account: DriveAccount; onOpenSettings: () => void }) {
  const state = useDriveAccount(account);
  const signedIn = state.phase === 'signed_in';
  const status = (() => {
    switch (state.phase) {
      case 'signed_in':
        return 'Google Drive connected';
      case 'signing_in':
        return 'Waiting for Google…';
      case 'error':
        return 'Sign-in needs attention';
      default:
        return 'Google Drive';
    }
  })();
  return (
    <button
      type="button"
      onClick={onOpenSettings}
      aria-label="Account — open Settings"
      className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-left transition-colors duration-150 ease-standard hover:bg-surface-2"
    >
      <span
        className={cx(
          'grid size-8 shrink-0 place-items-center rounded-lg border border-line',
          signedIn ? 'bg-accent-soft text-accent' : 'text-fg-subtle',
        )}
        aria-hidden="true"
      >
        {signedIn && state.email ? (
          <span className="text-xs font-semibold">{state.email.slice(0, 1).toUpperCase()}</span>
        ) : (
          <GoogleLogo size={15} weight="bold" />
        )}
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[13px] font-medium text-fg">
          {signedIn && state.email ? state.email : 'Not signed in'}
        </span>
        <span className="block truncate text-xs text-fg-subtle">{status}</span>
      </span>
    </button>
  );
}

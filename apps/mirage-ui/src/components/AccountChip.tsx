import { ArrowClockwise, GoogleLogo, Info, SignOut, SpinnerGap, UserCircle, X } from '@phosphor-icons/react';
import { useState } from 'react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';
import { Button } from '../ui';

function refreshedLabel(issued?: number): string {
  if (!issued) return '';
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - issued) / 60));
  return minutes <= 1 ? 'Token refreshed just now' : `Token refreshed ${minutes} min ago`;
}

/** Full Google account control: status, sign-in/out, and account switching. */
export function AccountChip({ account, onChanged, unavailable }: { account: DriveAccount; onChanged?: () => void; unavailable?: boolean }) {
  if (unavailable) {
    return (
      <div aria-live="polite">
        <div className="flex items-center gap-2.5">
          <span className="grid size-8 shrink-0 place-items-center rounded-lg border border-line text-fg-subtle" aria-hidden="true">
            <GoogleLogo size={15} weight="bold" />
          </span>
          <span className="text-xs text-fg-subtle">Unavailable</span>
        </div>
      </div>
    );
  }
  return <AccountChipInner account={account} onChanged={onChanged} />;
}

function AccountChipInner({ account, onChanged }: { account: DriveAccount; onChanged?: () => void }) {
  const state = useDriveAccount(account);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await work();
      onChanged?.();
    } finally {
      setBusy(false);
      setConfirming(false);
    }
  };

  const status = (() => {
    switch (state.phase) {
      case 'signed_in': return 'Connected to Google Drive';
      case 'signing_in': return 'Waiting for Google…';
      case 'error': return state.error ?? 'Sign-in needs attention.';
      default: return 'Not signed in to Google Drive';
    }
  })();
  const refreshed = state.phase === 'signed_in' ? refreshedLabel(state.issuedUnixSeconds) : '';

  return (
    <div aria-live="polite">
      <div className="flex items-center gap-2.5">
        <span className={`grid size-8 shrink-0 place-items-center rounded-lg border border-line ${state.phase === 'signed_in' ? 'bg-accent-soft text-accent' : 'text-fg-subtle'}`} aria-hidden="true">
          {state.phase === 'signed_in' ? <UserCircle size={16} weight="duotone" /> : <GoogleLogo size={15} weight="bold" />}
        </span>
        <div className="min-w-0 flex-1">
          {state.phase === 'signed_in' && state.email
            ? <span className="block truncate text-[13px] font-medium text-fg" title={state.email}>{state.email}</span>
            : <span className="block truncate text-[13px] font-medium text-fg">Google Drive</span>}
          <span className="block truncate text-xs text-fg-subtle" title={refreshed || undefined}>{status}</span>
        </div>
        {state.phase === 'signing_in'
          ? <SpinnerGap size={15} className="animate-spin text-fg-muted" aria-hidden="true" />
          : null}
      </div>

      {state.phase === 'error' && state.error && (
        <p className="mt-2.5 flex items-start gap-1.5 text-xs leading-relaxed text-danger" role="alert">
          <Info size={13} className="mt-0.5 shrink-0" aria-hidden="true" /> {state.error}
        </p>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        {state.phase === 'signed_out' && (
          <Button size="sm" disabled={busy} onClick={() => void run(() => account.signIn())}>
            Sign in
          </Button>
        )}
        {state.phase === 'signing_in' && (
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => void run(() => account.cancelSignIn())}>
            Cancel
          </Button>
        )}
        {state.phase === 'error' && (
          <Button size="sm" disabled={busy} onClick={() => void run(() => account.signIn())}>
            Try again
          </Button>
        )}
        {state.phase === 'signed_in' && !confirming && (
          <>
            <Button variant="ghost" size="sm" icon={<ArrowClockwise size={13} />} disabled={busy} onClick={() => void run(() => account.switchAccount())}>
              Switch account
            </Button>
            <Button variant="ghost" size="sm" icon={<SignOut size={13} />} disabled={busy} onClick={() => setConfirming(true)}>
              Sign out
            </Button>
          </>
        )}
      </div>

      {confirming && (
        <div className="mt-3 rounded-xl border border-line bg-surface-2 p-3.5" role="alertdialog" aria-label="Confirm sign out">
          <p className="text-xs leading-relaxed text-fg-muted">
            Sign out of {state.email ?? 'Google Drive'}? Your drives stay mounted until the service
            restarts, but new uploads and cold reads will fail until you sign in again.
            Files on Drive are not deleted.
          </p>
          <div className="mt-3 flex flex-wrap gap-2">
            <Button variant="danger" size="sm" disabled={busy} onClick={() => void run(() => account.signOut())}>
              Sign out
            </Button>
            <Button variant="ghost" size="sm" icon={<X size={13} />} disabled={busy} onClick={() => setConfirming(false)}>
              Keep signed in
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

import { ArrowClockwise, GoogleLogo, Info, SignOut, SpinnerGap, UserCircle, X } from '@phosphor-icons/react';
import { useState } from 'react';
import type { DriveAccount } from '../api/account';
import { useDriveAccount } from '../api/account';

function refreshedLabel(issued?: number): string {
  if (!issued) return '';
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - issued) / 60));
  return minutes <= 1 ? 'Token refreshed just now' : `Token refreshed ${minutes} min ago`;
}

/** Always-visible Google account control for the sidebar. */
export function AccountChip({ account, onChanged }: { account: DriveAccount; onChanged?: () => void }) {
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
      case 'signed_in': return refreshedLabel(state.issuedUnixSeconds) || 'Connected to Google Drive';
      case 'signing_in': return 'Waiting for Google…';
      case 'error': return state.error ?? 'Sign-in needs attention.';
      default: return 'Not signed in to Google Drive';
    }
  })();

  return (
    <div className="account-chip" aria-live="polite">
      <div className="account-chip-row">
        <span className={`account-chip-icon${state.phase === 'signed_in' ? ' is-signed-in' : ''}`} aria-hidden="true">
          {state.phase === 'signed_in' ? <UserCircle size={18} weight="duotone" /> : <GoogleLogo size={17} weight="bold" />}
        </span>
        <div className="account-chip-text">
          {state.phase === 'signed_in' && state.email
            ? <span className="account-chip-email" title={state.email}>{state.email}</span>
            : <span className="account-chip-email">Google Drive</span>}
          <small>{status}</small>
        </div>
        {state.phase === 'signing_in'
          ? <SpinnerGap size={16} className="refreshing" aria-hidden="true" />
          : null}
      </div>

      {state.phase === 'error' && state.error && (
        <p className="account-chip-error" role="alert"><Info size={14} /> {state.error}</p>
      )}

      <div className="account-chip-actions">
        {state.phase === 'signed_out' && (
          <button className="primary-button account-chip-button" disabled={busy}
            onClick={() => void run(() => account.signIn())}>
            Sign in
          </button>
        )}
        {state.phase === 'signing_in' && (
          <button className="quiet-button account-chip-button" disabled={busy}
            onClick={() => void run(() => account.cancelSignIn())}>
            Cancel
          </button>
        )}
        {state.phase === 'error' && (
          <button className="primary-button account-chip-button" disabled={busy}
            onClick={() => void run(() => account.signIn())}>
            Try again
          </button>
        )}
        {state.phase === 'signed_in' && !confirming && (
          <>
            <button className="quiet-button account-chip-button" disabled={busy}
              onClick={() => void run(() => account.switchAccount())}>
              <ArrowClockwise size={14} />Switch account
            </button>
            <button className="quiet-button account-chip-button" disabled={busy} onClick={() => setConfirming(true)}>
              <SignOut size={14} />Sign out
            </button>
          </>
        )}
      </div>

      {confirming && (
        <div className="account-chip-confirm" role="alertdialog" aria-label="Confirm sign out">
          <p>
            Sign out of {state.email ?? 'Google Drive'}? Your drives stay mounted until the service
            restarts, but new uploads and cold reads will fail until you sign in again.
            Files on Drive are not deleted.
          </p>
          <div className="account-chip-actions">
            <button className="primary-button account-chip-button" disabled={busy}
              onClick={() => void run(() => account.signOut())}>
              Sign out
            </button>
            <button className="quiet-button account-chip-button" disabled={busy} onClick={() => setConfirming(false)}>
              <X size={14} />Keep signed in
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

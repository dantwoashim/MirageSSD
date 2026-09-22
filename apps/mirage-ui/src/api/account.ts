import { useSyncExternalStore } from 'react';
import type { ServiceClient } from './client';
import type { DriveStatus } from '../models';

export type AccountPhase = 'signed_out' | 'signing_in' | 'signed_in' | 'error';

export type AccountSnapshot = {
  phase: AccountPhase;
  email?: string;
  issuedUnixSeconds?: number;
  error?: string;
};

const IDLE: DriveStatus = { authenticated: false, account_id: null, issued_unix_seconds: null, login: 'idle' };

function snapshotOf(status: DriveStatus | undefined): AccountSnapshot {
  if (!status) return { phase: 'signed_out' };
  if (status.login === 'in_flight') return { phase: 'signing_in' };
  if (typeof status.login === 'object' && status.login !== null && 'failed' in status.login) {
    return { phase: 'error', error: status.login.failed };
  }
  if (status.authenticated) {
    return {
      phase: 'signed_in',
      email: status.account_id ?? undefined,
      issuedUnixSeconds: status.issued_unix_seconds ?? undefined,
    };
  }
  return { phase: 'signed_out' };
}

/**
 * One shared Drive sign-in state per client: the sidebar chip, the setup
 * wizard, and the drive card all subscribe to the same poll + actions.
 */
export function createDriveAccount(client: ServiceClient) {
  let status: DriveStatus | undefined;
  let listeners: Array<() => void> = [];
  let snapshot: AccountSnapshot = snapshotOf(undefined);
  let version = 0;
  let timer: number | undefined;

  const publish = (next: DriveStatus | undefined) => {
    status = next ?? IDLE;
    const candidate = snapshotOf(status);
    // Only notify when the phase-visible fields actually change.
    if (candidate.phase === snapshot.phase && candidate.email === snapshot.email
        && candidate.error === snapshot.error && candidate.issuedUnixSeconds === snapshot.issuedUnixSeconds) {
      return;
    }
    snapshot = candidate;
    version++;
    listeners.forEach((listener) => listener());
  };

  const refresh = async () => {
    try {
      publish(await client.driveStatus());
    } catch (failure) {
      publish({ ...IDLE, login: { failed: failure instanceof Error ? failure.message : String(failure) } });
    }
  };

  const ensurePolling = () => {
    if (timer === undefined) {
      void refresh();
      if (typeof window !== 'undefined') {
        timer = window.setInterval(() => void refresh(), 60_000);
      }
    }
  };

  const action = async (run: () => Promise<unknown>) => {
    try {
      await run();
    } finally {
      await refresh();
    }
  };

  return {
    subscribe(listener: () => void) {
      listeners = [...listeners, listener];
      ensurePolling();
      return () => { listeners = listeners.filter((item) => item !== listener); };
    },
    getVersion: () => version,
    getSnapshot: () => snapshot,
    refresh,
    signIn: () => action(() => client.driveLogin()),
    signOut: () => action(() => client.driveLogout()),
    switchAccount: async () => {
      await client.driveLogout().catch(() => {});
      await refresh();
      return action(() => client.driveLogin());
    },
    cancelSignIn: () => action(() => client.driveLoginCancel()),
  };
}

export type DriveAccount = ReturnType<typeof createDriveAccount>;

export function useDriveAccount(account: DriveAccount): AccountSnapshot {
  useSyncExternalStore(account.subscribe, account.getVersion, account.getVersion);
  return account.getSnapshot();
}

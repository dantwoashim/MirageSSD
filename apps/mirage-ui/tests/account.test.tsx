import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import { createDriveAccount, useDriveAccount, type DriveAccount } from '../src/api/account';
import { AccountChip } from '../src/components/AccountChip';
import type { LocalBridge } from '../src/api/client';
import { ServiceClient } from '../src/api/client';
import type { DriveStatus } from '../src/models';

function clientWith(status: () => DriveStatus, calls?: { log: string[] }): ServiceClient {
  const bridge: LocalBridge = {
    invoke: async () => ({ protocol_version: 3, request_id: 0, body: { kind: 'json', value: {} } }),
    apiGet: async () => status(),
    apiPost: async (path) => { calls?.log.push(path); return {}; },
  };
  return new ServiceClient(bridge);
}

const SIGNED_IN: DriveStatus = {
  authenticated: true,
  account_id: 'friend@example.com',
  issued_unix_seconds: Math.floor(Date.now() / 1000) - 120,
  login: 'idle',
};

describe('drive account store', () => {
  it('moves signed out → signing in → signed in → signed out', async () => {
    let status: DriveStatus = { authenticated: false, account_id: null, issued_unix_seconds: null, login: 'idle' };
    const calls = { log: [] as string[] };
    const account = createDriveAccount(clientWith(() => status, calls));

    await account.refresh();
    expect(account.getSnapshot().phase).toBe('signed_out');

    status = { ...status, login: 'in_flight' };
    await account.signIn();
    expect(calls.log).toContain('/api/drive/login');
    expect(account.getSnapshot().phase).toBe('signing_in');

    status = SIGNED_IN;
    await account.refresh();
    expect(account.getSnapshot().phase).toBe('signed_in');
    expect(account.getSnapshot().email).toBe('friend@example.com');

    status = { authenticated: false, account_id: null, issued_unix_seconds: null, login: 'idle' };
    await account.signOut();
    expect(calls.log).toContain('/api/drive/logout');
    expect(account.getSnapshot().phase).toBe('signed_out');
  });

  it('surfaces a neutral sign-in error and supports cancel', async () => {
    const calls = { log: [] as string[] };
    const account = createDriveAccount(clientWith(() => ({
      authenticated: false,
      account_id: null,
      issued_unix_seconds: null,
      login: { failed: 'Google declined access for this account. Try again or use a different account.' },
    }), calls));
    await account.refresh();
    expect(account.getSnapshot().phase).toBe('error');
    expect(account.getSnapshot().error).toContain('declined');

    await account.cancelSignIn();
    expect(calls.log).toContain('/api/drive/login/cancel');
  });
});

function Chip({ account }: { account: DriveAccount }) {
  useDriveAccount(account);
  return <AccountChip account={account} />;
}

describe('account chip', () => {
  const render = async (status: DriveStatus) => {
    const account = createDriveAccount(clientWith(() => status));
    await account.refresh();
    return renderToStaticMarkup(createElement(Chip, { account }));
  };

  it('offers sign-in when signed out', async () => {
    const html = await render({ authenticated: false, account_id: null, issued_unix_seconds: null, login: 'idle' });
    expect(html).toContain('Not signed in to Google Drive');
    expect(html).toContain('Sign in');
    expect(html).toContain('aria-live="polite"');
  });

  it('shows the waiting state while Google decides', async () => {
    const html = await render({ authenticated: false, account_id: null, issued_unix_seconds: null, login: 'in_flight' });
    expect(html).toContain('Waiting for Google');
    expect(html).toContain('Cancel');
  });

  it('shows the account and actions when signed in', async () => {
    const html = await render(SIGNED_IN);
    expect(html).toContain('friend@example.com');
    expect(html).toContain('Switch account');
    expect(html).toContain('Sign out');
  });

  it('shows the error and a retry', async () => {
    const html = await render({ authenticated: false, account_id: null, issued_unix_seconds: null, login: { failed: 'Try again later.' } });
    expect(html).toContain('Try again later.');
    expect(html).toContain('Try again');
  });
});

import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import type { DriveAccount } from '../src/api/account';
import type { ServiceClient } from '../src/api/client';
import type { UpdateCheck } from '../src/models';
import { SettingsView } from '../src/views/Settings';

const account: DriveAccount = {
  getVersion: () => 0,
  getSnapshot: () => ({ phase: 'signed_out' }),
  subscribe: () => () => {},
  refresh: async () => {},
  signIn: async () => {},
  signOut: async () => {},
  cancelSignIn: async () => {},
  switchAccount: async () => {},
};

const client = {} as ServiceClient;

const update: UpdateCheck = {
  checked: true,
  checked_at: 1_759_632_000,
  current: '0.1.17',
  channel: 'preview',
  latest: 'v0.1.18',
  url: 'https://github.com/dantwoashim/MirageSSD/releases/tag/v0.1.18',
  update_available: true,
};

describe('settings', () => {
  it('offers the three theme options', () => {
    const html = renderToStaticMarkup(createElement(SettingsView, { client, account, updateInfo: update }));
    expect(html).toContain('System');
    expect(html).toContain('Light');
    expect(html).toContain('Dark');
  });

  it('links to the download when an update is available', () => {
    const html = renderToStaticMarkup(createElement(SettingsView, { client, account, updateInfo: update }));
    expect(html).toContain('Download');
    expect(html).toContain(update.url);
  });

  it('includes the diagnostics action', () => {
    const html = renderToStaticMarkup(createElement(SettingsView, { client, account, updateInfo: update }));
    expect(html).toContain('Collect diagnostics');
  });
});

import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import type { DriveAccount } from '../src/api/account';
import { Sidebar } from '../src/components/Sidebar';

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

const render = (showAdvanced: boolean) =>
  renderToStaticMarkup(createElement(Sidebar, {
    view: 'overview',
    onNavigate: () => {},
    showAdvanced,
    account,
    service: 'running',
  }));

describe('shell navigation', () => {
  it('renders the primary navigation', () => {
    const html = render(false);
    expect(html).toContain('Drives');
    expect(html).toContain('Settings');
    expect(html).toContain('Help');
  });

  it('hides advanced navigation until it applies', () => {
    const html = render(false);
    expect(html).not.toContain('Local storage');
    expect(html).not.toContain('Offline access');
    expect(html).not.toContain('Recovery');
  });

  it('shows advanced navigation when showAdvanced is true', () => {
    const html = render(true);
    expect(html).toContain('Local storage');
    expect(html).toContain('Offline access');
    expect(html).toContain('Versions');
    expect(html).toContain('Recovery');
    expect(html).toContain('Add a workspace');
    expect(html).toContain('Restore before uninstall');
  });
});

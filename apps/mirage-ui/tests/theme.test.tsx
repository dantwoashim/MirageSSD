import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { AccountChip } from '../src/components/AccountChip';
import { ThemeToggle } from '../src/components/ThemeToggle';
import type { DriveAccount } from '../src/api/account';

const account = {
  subscribe: () => () => {},
  getVersion: () => 0,
  getSnapshot: () => ({ phase: 'error' as const, error: 'The MirageSSD UI bridge token is missing or invalid. Reopen the app.' }),
  signIn: async () => {},
  signOut: async () => {},
  switchAccount: async () => {},
  cancelSignIn: async () => {},
  refresh: async () => {},
};

describe('theme and degraded states', () => {
  it('offers System/Light/Dark options', () => {
    const markup = renderToStaticMarkup(<ThemeToggle labelled />);
    expect(markup).toContain('System');
    expect(markup).toContain('Light');
    expect(markup).toContain('Dark');
  });

  it('renders a single muted chip when the bridge is unavailable', () => {
    const markup = renderToStaticMarkup(<AccountChip account={account} unavailable />);
    expect(markup).toContain('Unavailable');
    expect(markup).not.toContain('bridge token');
    expect(markup).not.toContain('Sign in');
    expect(markup).not.toContain('Try again');
  });
});

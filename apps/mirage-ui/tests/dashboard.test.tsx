import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import { Dashboard } from '../src/views/Dashboard';
import type { RepositoryState } from '../src/models';

const repository: RepositoryState = {
  id: '0'.repeat(32),
  name: 'Existing',
  generation: 1,
  commit: 'ab'.repeat(16),
  state: 'ready_mounted',
  mounted: true,
  physicalBytes: 1024,
  logicalBytes: 2048,
  backendHealth: 'online',
  lastSealViolations: 0,
  origin: 'drive',
  volumeMode: 'managed',
  mountPath: 'M:\\',
};

describe('dashboard drive creation entry points', () => {
  it('shows a create-drive call to action on the empty state', () => {
    const html = renderToStaticMarkup(createElement(Dashboard, {
      repositories: [],
      onSelect: () => {},
      onSetup: () => {},
    }));
    expect(html).toContain('Create your drive');
    expect(html).toContain('Sign in with Google');
  });

  it('offers Add a drive alongside existing workspaces', () => {
    const html = renderToStaticMarkup(createElement(Dashboard, {
      repositories: [repository],
      selectedId: repository.id,
      onSelect: () => {},
      onSetup: () => {},
    }));
    expect(html).toContain('Add a drive');
    expect(html).toContain('Existing');
  });

  it('shows an uploading indicator for a managed drive with pending bytes', () => {
    const uploading = { ...repository, pendingBytes: Math.round(1.37 * 1024 ** 3) };
    const html = renderToStaticMarkup(createElement(Dashboard, {
      repositories: [uploading],
      selectedId: repository.id,
      onSelect: () => {},
      onSetup: () => {},
    }));
    expect(html).toContain('Uploading');
  });
});

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
};

describe('dashboard drive creation entry points', () => {
  it('shows a create-drive call to action on the empty state', () => {
    const html = renderToStaticMarkup(createElement(Dashboard, {
      repositories: [],
      onSelect: () => {},
      onRefresh: () => {},
      onSetup: () => {},
    }));
    expect(html).toContain('Create your drive');
    expect(html).toContain('Sign in with Google and create an encrypted drive');
  });

  it('offers Add a drive alongside existing workspaces', () => {
    const html = renderToStaticMarkup(createElement(Dashboard, {
      repositories: [repository],
      selectedId: repository.id,
      onSelect: () => {},
      onRefresh: () => {},
      onSetup: () => {},
    }));
    expect(html).toContain('Add a drive');
    expect(html).toContain('Existing');
  });
});

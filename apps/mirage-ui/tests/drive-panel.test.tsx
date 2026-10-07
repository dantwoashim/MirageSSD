import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import type { ServiceClient } from '../src/api/client';
import type { RepositoryState } from '../src/models';
import { DrivePanel } from '../src/views/drive/DrivePanel';

const client = {} as ServiceClient;

const drive = (overrides: Partial<RepositoryState> = {}): RepositoryState => ({
  id: '0'.repeat(32),
  name: 'MirageSSD',
  state: 'ready_mounted',
  generation: 0,
  commit: 'ab'.repeat(16),
  mounted: true,
  physicalBytes: 18 * 1024 ** 3,
  logicalBytes: 212 * 1024 ** 3,
  backendHealth: 'online',
  lastSealViolations: 0,
  pendingBytes: 0,
  publishedBytes: 212 * 1024 ** 3,
  pendingOperations: 0,
  origin: 'drive',
  volumeMode: 'managed',
  mountPath: 'M:\\',
  cacheDiskRoot: 'C:\\',
  cacheDiskFreeBytes: 143 * 1024 ** 3,
  cacheDiskTotalBytes: 476 * 1024 ** 3,
  cacheDiskFloorBytes: 47 * 1024 ** 3,
  ...overrides,
});

const render = (repository: RepositoryState) =>
  renderToStaticMarkup(createElement(DrivePanel, {
    client,
    repository,
    busy: false,
    onToggleMount: () => {},
    onChanged: () => {},
  }));

describe('drive panel', () => {
  it('offers Explorer and shows upload state on a mounted drive with pending bytes', () => {
    const html = render(drive({ pendingBytes: Math.round(1.37 * 1024 ** 3) }));
    expect(html).toContain('Open in Explorer');
    expect(html).toContain('Uploading');
  });

  it('offers Connect on an unmounted drive', () => {
    expect(render(drive({ state: 'ready_unmounted', mounted: false }))).toContain('Connect');
  });

  it('always includes the remove action', () => {
    expect(render(drive())).toContain('Remove from this PC');
    expect(render(drive({ state: 'ready_unmounted', mounted: false }))).toContain('Remove from this PC');
  });

  it('shows a disk-relative storage meter when the cache disk is known', () => {
    const html = render(drive());
    expect(html).toContain('Storage on C:\\');
    expect(html).toContain('Other files');
    expect(html).toContain('Free');
  });
});

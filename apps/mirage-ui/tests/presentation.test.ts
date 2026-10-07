import { describe, expect, it } from 'vitest';
import { bridgeSession } from '../src/api/session';
import {
  connectionStatus,
  driveLetter,
  isManagedDrive,
  operationMessage,
  readinessFromPlan,
  repositoryScope,
  uploadStatus,
} from '../src/presentation';
import { formatBytes } from '../src/components/CapsuleBreakdown';
import type { RepositoryState } from '../src/models';

describe('offline readiness', () => {
  it('preserves zero missing bytes and zero hard-set bytes', () => {
    expect(readinessFromPlan({ capsule_id: 'a', total_bytes: 4096, missing_bytes: 0, hard_set_bytes: 0 })).toMatchObject({ missingBytes: 0, hardSetBytes: 0, state: 'materializing' });
  });
  it('never approves missing, negative or non-finite evidence', () => {
    for (const missing_bytes of [-1, NaN, Infinity]) expect(() => readinessFromPlan({ capsule_id: 'a', total_bytes: 4096, missing_bytes })).toThrow();
    expect(() => readinessFromPlan({ capsule_id: 'a' })).toThrow();
    expect(readinessFromPlan({ capsule_id: 'a', state: 'sealed_ready', total_bytes: 4096, missing_bytes: 3 }).state).toBe('not_ready');
  });
  it('scopes preparation to the drive and its committed version', () => {
    const repository = { id: 'a', generation: 1, commit: 'one' } as RepositoryState;
    expect(repositoryScope(repository)).not.toBe(repositoryScope({ ...repository, id: 'b' }));
    expect(repositoryScope(repository)).not.toBe(repositoryScope({ ...repository, generation: 2 }));
    expect(repositoryScope(repository)).not.toBe(repositoryScope({ ...repository, commit: 'two' }));
  });
  it('does not call an accepted operation completed', () => {
    expect(operationMessage('Preparation', { accepted: true, operation_id: 4 })).toContain('not yet confirmed');
  });
});

describe('desktop session', () => {
  it('survives reload in this tab and replaces a previous capability', () => {
    const values = new Map<string, string>();
    const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); } };
    expect(bridgeSession(`#${'a'.repeat(64)}`, storage).removeHash).toBe(true);
    expect(bridgeSession('', storage).token).toBe('a'.repeat(64));
    bridgeSession(`#${'b'.repeat(64)}`, storage);
    expect(bridgeSession('', storage).token).toBe('b'.repeat(64));
    expect(bridgeSession('#invalid', storage).token).toBe('');
  });
  it('keeps the launch fragment when browser storage is blocked', () => {
    const storage = { getItem: () => { throw new Error('denied'); }, setItem: () => { throw new Error('denied'); } };
    expect(bridgeSession(`#${'a'.repeat(64)}`, storage)).toEqual({ token: 'a'.repeat(64), removeHash: false });
    expect(bridgeSession('', storage).token).toBe('');
  });
});

it('displays unknown byte measurements honestly', () => {
  for (const value of [null, NaN, -1]) expect(formatBytes(value)).toBe('Not measured');
  expect(formatBytes(0)).toBe('0 B');
});

const drive = (overrides: Partial<RepositoryState> = {}): RepositoryState => ({
  id: '0'.repeat(32),
  name: 'Drive',
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
  ...overrides,
});

describe('isManagedDrive', () => {
  it('is true for a drive-origin managed volume only', () => {
    expect(isManagedDrive(drive())).toBe(true);
    expect(isManagedDrive(drive({ origin: 'local' }))).toBe(false);
    expect(isManagedDrive(drive({ volumeMode: 'engine' }))).toBe(false);
    expect(isManagedDrive(undefined)).toBe(false);
  });
});

describe('driveLetter', () => {
  it('returns the letter for mount paths like M:\\ and M:', () => {
    expect(driveLetter(drive({ mountPath: 'M:\\' }))).toBe('M');
    expect(driveLetter(drive({ mountPath: 'M:' }))).toBe('M');
  });
  it('is undefined without a mount path', () => {
    expect(driveLetter(drive({ mountPath: undefined }))).toBeUndefined();
    expect(driveLetter(undefined)).toBeUndefined();
  });
});

describe('connectionStatus', () => {
  const cases: Array<[string, string, string]> = [
    ['ready_mounted', 'ok', 'Connected'],
    ['playing_sealed', 'ok', 'In use'],
    ['playing_balanced', 'ok', 'In use'],
    ['ready_unmounted', 'neutral', 'Disconnected'],
    ['mounting', 'busy', 'Connecting'],
    ['importing', 'busy', 'Importing'],
    ['uploading_base', 'busy', 'Uploading'],
    ['verifying_base', 'busy', 'Verifying'],
    ['admitting_session', 'busy', 'Preparing'],
    ['updating', 'busy', 'Updating'],
    ['recovering', 'busy', 'Recovering'],
    ['degraded', 'warn', 'Limited'],
    ['conflicted', 'danger', 'Needs attention'],
    ['error', 'danger', 'Needs attention'],
    ['uninitialized', 'neutral', 'Not set up'],
  ];
  for (const [state, tone, label] of cases) {
    it(`${state} maps to ${tone} "${label}"`, () => {
      expect(connectionStatus(drive({ state }))).toEqual({ tone, label });
    });
  }
  it('humanizes unknown states', () => {
    expect(connectionStatus(drive({ state: 'some_new_state' }))).toEqual({ tone: 'neutral', label: 'Some New State' });
  });
});

describe('uploadStatus', () => {
  it('reports unavailable when pending bytes are unknown', () => {
    expect(uploadStatus(drive({ pendingBytes: null }))).toEqual({ tone: 'neutral', label: 'Upload status unavailable' });
    expect(uploadStatus(drive({ pendingBytes: undefined }))).toEqual({ tone: 'neutral', label: 'Upload status unavailable' });
  });
  it('reports done when nothing is pending', () => {
    expect(uploadStatus(drive({ pendingBytes: 0 }))).toEqual({ tone: 'ok', label: 'Everything is uploaded to Google Drive' });
  });
  it('reports the pending amount when uploads remain', () => {
    const status = uploadStatus(drive({ pendingBytes: Math.round(1.37 * 1024 ** 3) }));
    expect(status.tone).toBe('busy');
    expect(status.label).toContain('Uploading');
    expect(status.label).toContain('Google Drive');
  });
});

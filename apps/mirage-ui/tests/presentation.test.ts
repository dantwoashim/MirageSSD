import { describe, expect, it } from 'vitest';
import { bridgeSession } from '../src/api/session';
import { operationMessage, readinessFromPlan, repositoryScope } from '../src/presentation';
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

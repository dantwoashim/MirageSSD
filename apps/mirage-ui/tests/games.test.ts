import { expect, it, vi } from 'vitest';
import { ServiceClient } from '../src/api/client';
import { validGameBudgets } from '../src/views/Games';

it('uses the actual authenticated desktop game endpoints and preserves in-flight status on reopen', async () => {
  const apiGet = vi.fn().mockResolvedValue({ registered: true, busy: true, phase: 'playing' });
  const apiPost = vi.fn().mockResolvedValue({ started: true });
  const bridge = { invoke: vi.fn(), apiGet, apiPost };
  const first = new ServiceClient(bridge);
  await first.gameAction('install', { root: 'D:\\Games\\New', retention: 'disposable' });
  expect(apiPost).toHaveBeenCalledWith('/api/game/action', { operation: 'install', spec: { root: 'D:\\Games\\New', retention: 'disposable' } });
  expect(await new ServiceClient(bridge).gameStatus()).toEqual({ registered: true, busy: true, phase: 'playing' });
  expect(apiGet).toHaveBeenCalledWith('/api/game/status');
});
it('surfaces detached worker setup failure and rejects an action without acceptance', async () => {
  const client = new ServiceClient({ invoke: vi.fn(), apiGet: vi.fn().mockResolvedValue({ error: 'native verification failed' }), apiPost: vi.fn().mockResolvedValue({ error: 'already playing' }) });
  await expect(client.gameStatus()).rejects.toThrow('native verification failed');
  await expect(client.gameAction('repair')).rejects.toThrow('already playing');
  await expect(new ServiceClient({ invoke: vi.fn(), apiPost: vi.fn().mockResolvedValue({ started: false }) }).gameAction('play')).rejects.toThrow('did not start');
});
it('keeps cleanup-pending a recoverable status instead of reporting readiness', async () => {
  const client = new ServiceClient({ invoke: vi.fn(), apiGet: vi.fn().mockResolvedValue({ registered: true, busy: false, phase: 'cleanup_pending', error: 'locked chunk' }) });
  expect((await client.gameStatus()).phase).toBe('cleanup_pending');
});
it('routes policy, rollback and explicit retirement to real async operations', async () => {
  const apiPost = vi.fn().mockResolvedValue({ started: true });
  const client = new ServiceClient({ invoke: vi.fn(), apiPost });
  await client.gameAction('policy', { retention: 'disposable' });
  await client.gameAction('rollback'); await client.gameAction('retire');
  expect(apiPost).toHaveBeenNthCalledWith(1, '/api/game/action', { operation: 'policy', spec: { retention: 'disposable' } });
  expect(apiPost).toHaveBeenNthCalledWith(2, '/api/game/action', { operation: 'rollback', spec: undefined });
  expect(apiPost).toHaveBeenNthCalledWith(3, '/api/game/action', { operation: 'retire', spec: undefined });
});
it('rejects nonfinite, fractional, negative and over-allowance budgets', () => {
  for (const [total, cache] of [['Infinity', '4'], ['40', 'NaN'], ['0', '1'], ['40', '-1'], ['40', '41'], ['1025', '4'], ['40.5', '4']]) expect(validGameBudgets(total, cache)).toBe(false);
  expect(validGameBudgets('40', '4')).toBe(true);
});

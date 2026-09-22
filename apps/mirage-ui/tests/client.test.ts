import { describe, expect, it } from 'vitest';
import { ProtocolMismatch, ServiceClient } from '../src/api/client';
import type { LocalBridge } from '../src/api/client';
import type { Request, Response } from '../src/models';

function response(request: Request, protocolVersion = 3): Response {
  return {
    protocol_version: protocolVersion,
    request_id: request.request_id,
    body: {
      kind: 'json',
      value: { service: 'running', configured: false, repositories: [] },
    },
  };
}

describe('service client', () => {
  it('rejects mismatched service versions', async () => {
    const bridge: LocalBridge = { invoke: async (request) => response(request, 2) };
    await expect(new ServiceClient(bridge).snapshot()).rejects.toBeInstanceOf(ProtocolMismatch);
  });

  it('normalizes a correlated status response', async () => {
    const bridge: LocalBridge = { invoke: async (request) => response(request) };
    await expect(new ServiceClient(bridge).snapshot()).resolves.toMatchObject({
      protocolVersion: 3,
      configured: false,
      repositories: [],
    });
  });

  it('rejects malformed status and unknown result kinds', async () => {
    const bridge: LocalBridge = { invoke: async (request) => ({ ...response(request), body: { kind: 'json', value: { repositories: null } } }) };
    await expect(new ServiceClient(bridge).snapshot()).rejects.toBeInstanceOf(ProtocolMismatch);
    const unknown: LocalBridge = { invoke: async (request) => ({ ...response(request), body: { kind: 'future_kind', value: {} } } as unknown as Response) };
    await expect(new ServiceClient(unknown).snapshot()).rejects.toBeInstanceOf(ProtocolMismatch);
  });

  it('keeps absent publication state unknown and preserves measured zero', async () => {
    const bridge: LocalBridge = { invoke: async (request) => ({ ...response(request), body: { kind: 'json', value: {
      service: 'running', configured: true, repositories: [
        { repository_id: 'a', display_name: 'A', state: 'ready_mounted', active_generation: 0, active_commit: null, unpublished_payload_bytes: 0 },
        { repository_id: 'b', display_name: 'B', state: 'ready_mounted', active_generation: 0, active_commit: null },
      ],
    } } }) };
    const snapshot = await new ServiceClient(bridge).snapshot();
    expect(snapshot.repositories[0].pendingBytes).toBe(0);
    expect(snapshot.repositories[1].pendingBytes).toBeNull();
    expect(snapshot.repositories[1].backendHealth).toBe('unknown');
  });

  it('never sends an unsealed seamless launch', async () => {
    let calls = 0;
    const bridge: LocalBridge = {
      invoke: async (request) => {
        calls++;
        return response(request);
      },
    };
    await expect(new ServiceClient(bridge).launch('0'.repeat(32), 'verified_local', 'materializing')).rejects.toThrow();
    expect(calls).toBe(0);
  });

  it('retries Drive materialization through the credential broker without browser secrets', async () => {
    let normalCalls = 0;
    let driveRequest: Request | undefined;
    const bridge: LocalBridge = {
      invoke: async (request) => {
        normalCalls++;
        return {
          protocol_version: 3,
          request_id: request.request_id,
          body: { kind: 'error', value: { code: 'MIRAGE_BACKEND_UNAUTHENTICATED', message: 'Drive login required' } },
        };
      },
      invokeDrive: async (request) => {
        driveRequest = request;
        return {
          protocol_version: 3,
          request_id: request.request_id,
          body: { kind: 'json', value: { complete: true } },
        };
      },
    };

    await expect(new ServiceClient(bridge).materialize('0'.repeat(32), '1'.repeat(32))).resolves.toEqual({ complete: true });
    expect(normalCalls).toBe(1);
    expect(driveRequest?.command.command).toBe('materialize');
    expect(JSON.stringify(driveRequest)).not.toContain('drive_access_token');
  });

  it('returns the free drive letters and disk defaults for the wizard', async () => {
    let path = '';
    const bridge: LocalBridge = {
      invoke: async (request) => response(request),
      apiGet: async (requested) => {
        path = requested;
        return {
          disks: [{ volume_root: 'C:\\', total_bytes: 1000, free_bytes: 500 }],
          state_volume: { volume_root: 'C:\\', total_bytes: 1000, free_bytes: 500 },
          default_letter: 'M',
          default_budget_bytes: 64 * 1024 ** 3,
          free_letters: ['D', 'E', 'M', 'N'],
        };
      },
    };
    const disks = await new ServiceClient(bridge).disks();
    expect(path).toBe('/api/disks');
    expect(disks.free_letters).toEqual(['D', 'E', 'M', 'N']);
    expect(disks.default_letter).toBe('M');
  });

  it('surfaces volume creation failures from the local bridge', async () => {
    const bridge: LocalBridge = {
      invoke: async (request) => response(request),
      apiPost: async () => ({ accepted: false, error: 'Drive letter M: is already in use.' }),
    };
    await expect(new ServiceClient(bridge).volumeCreate({ name: 'MirageSSD', letter: 'M', budget_bytes: 1024 })).rejects.toThrow('already in use');
  });
});

import {
  PROTOCOL_VERSION,
  type IpcCommand,
  type CapacityLease,
  type CapacityPlan,
  type DiskInfo,
  type DisksPayload,
  type DriveStatus,
  type Mode,
  type Request,
  type Response,
  type ServiceRepository,
  type ServiceSnapshot,
  type UpdateCheck,
  type VolumeCreateStatus,
  type VolumeSetCacheStatus,
} from '../models';
import { record } from '../presentation';

export interface LocalBridge {
  invoke(request: Request): Promise<Response>;
  invokeDrive?(request: Request): Promise<Response>;
  apiGet?(path: string): Promise<unknown>;
  apiPost?(path: string, body?: unknown): Promise<unknown>;
}

export class ProtocolMismatch extends Error {}
export class ServiceError extends Error {
  constructor(readonly code: string, message: string) {
    super(message);
  }
}

export class FetchBridge implements LocalBridge {
  constructor(private readonly token: string) {}

  async invoke(request: Request): Promise<Response> {
    return await this.post('/api/invoke', request);
  }

  async invokeDrive(request: Request): Promise<Response> {
    return await this.post('/api/invoke-drive', request);
  }

  async apiGet(path: string): Promise<unknown> {
    if (!/^[0-9a-f]{64}$/.test(this.token)) {
      throw new Error('The MirageSSD UI bridge token is missing or invalid. Reopen the app.');
    }
    const response = await fetch(path, {
      cache: 'no-store',
      credentials: 'omit',
      headers: { 'X-Mirage-Token': this.token },
      signal: AbortSignal.timeout(30_000),
    }).catch(() => {
      throw new Error('MirageSSD is not responding. Open the desktop app, then reconnect.');
    });
    if (!response.ok) throw new Error(`MirageSSD could not complete the request (${response.status}).`);
    return await response.json();
  }

  async apiPost(path: string, body?: unknown): Promise<unknown> {
    if (!/^[0-9a-f]{64}$/.test(this.token)) {
      throw new Error('The MirageSSD UI bridge token is missing or invalid. Reopen the app.');
    }
    const response = await fetch(path, {
      method: 'POST',
      cache: 'no-store',
      credentials: 'omit',
      headers: { 'Content-Type': 'application/json', 'X-Mirage-Token': this.token },
      body: JSON.stringify(body ?? {}),
      signal: AbortSignal.timeout(30_000),
    }).catch(() => {
      throw new Error('MirageSSD is not responding. Open the desktop app, then reconnect.');
    });
    if (!response.ok) throw new Error(`MirageSSD could not complete the request (${response.status}).`);
    return await response.json();
  }

  private async post(path: string, request: Request): Promise<Response> {
    if (!/^[0-9a-f]{64}$/.test(this.token)) {
      throw new Error('The MirageSSD UI bridge token is missing or invalid. Reopen the app.');
    }
    const response = await fetch(path, {
      method: 'POST',
      cache: 'no-store',
      credentials: 'omit',
      headers: {
        'Content-Type': 'application/json',
        'X-Mirage-Token': this.token,
      },
      body: JSON.stringify(request),
      signal: AbortSignal.timeout(60_000),
    }).catch((error: unknown) => {
      if (error instanceof Error && (error.name === 'TimeoutError' || error.name === 'AbortError')) {
        throw new Error('The connection timed out. Your operation may still be running. Refresh its status before trying again.');
      }
      throw new Error('MirageSSD is not responding. Open the desktop app, then reconnect.');
    });
    if (!response.ok) {
      throw new Error(response.status === 403
        ? 'This connection has expired. Reopen MirageSSD from the Start menu.'
        : `MirageSSD could not complete the request (${response.status}). Refresh its status before retrying.`);
    }
    return await response.json() as Response;
  }
}

export class ServiceClient {
  private requestId = 0;

  constructor(private readonly bridge: LocalBridge) {}

  async snapshot(): Promise<ServiceSnapshot> {
    const value = await this.invoke({ command: 'status' });
    if (!record(value) || typeof value.service !== 'string' || typeof value.configured !== 'boolean' || !Array.isArray(value.repositories)) {
      throw new ProtocolMismatch('The service returned an incomplete drive list. Reopen MirageSSD after updating it.');
    }
    return {
      protocolVersion: PROTOCOL_VERSION,
      service: value.service,
      configured: value.configured,
      repositories: value.repositories.map(normalizeRepository),
    };
  }

  async detail(repositoryId: string) {
    const value = await this.invoke({ command: 'repository_detail', body: { repository_id: repositoryId } });
    const normalized = normalizeRepository(value);
    if (normalized.id !== repositoryId) throw new ProtocolMismatch('The service returned a different drive. Refresh before continuing.');
    return normalized;
  }

  async mount(repositoryId: string, generation: number): Promise<unknown> {
    return await this.invoke({
      command: 'mount',
      body: { repository_id: repositoryId, generation },
    });
  }

  async unmount(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'unmount', body: { repository_id: repositoryId } });
  }

  async repositoryUnregister(repositoryId: string, forceUnmount: boolean, discardUnpublished: boolean): Promise<unknown> {
    return await this.invoke({
      command: 'repository_unregister',
      body: { repository_id: repositoryId, force_unmount: forceUnmount, discard_unpublished: discardUnpublished },
    });
  }

  async plan(repositoryId: string): Promise<unknown> {
    return await this.invoke({
      command: 'plan',
      body: { repository_id: repositoryId, full_volume: true },
    });
  }

  async capacityPlan(repositoryId: string, requestedBytes: number, authenticateDrive = false): Promise<CapacityPlan> {
    return await this.invoke({
      command: 'capacity_plan',
      body: { repository_id: repositoryId, requested_bytes: requestedBytes },
    }, authenticateDrive) as CapacityPlan;
  }

  async capacityAcquire(repositoryId: string, requestedBytes: number, authenticateDrive = false): Promise<CapacityLease> {
    return await this.invoke({
      command: 'capacity_acquire',
      body: { repository_id: repositoryId, requested_bytes: requestedBytes, lifetime_seconds: 21_600 },
    }, authenticateDrive) as CapacityLease;
  }

  async capacityRelease(repositoryId: string, leaseId: string): Promise<unknown> {
    return await this.invoke({
      command: 'capacity_release',
      body: { repository_id: repositoryId, lease_id: leaseId },
    });
  }

  async materialize(repositoryId: string, capsuleId: string): Promise<unknown> {
    const command: IpcCommand = {
      command: 'materialize', body: { repository_id: repositoryId, capsule_id: capsuleId },
    };
    try {
      return await this.invoke(command);
    } catch (error) {
      if (!(error instanceof ServiceError) || error.code !== 'MIRAGE_BACKEND_UNAUTHENTICATED') throw error;
      return await this.invoke(command, true);
    }
  }

  async admit(repositoryId: string, capsuleId: string): Promise<unknown> {
    return await this.invoke({ command: 'admit', body: { repository_id: repositoryId, capsule_id: capsuleId } });
  }

  async nativeActivate(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'native_activate', body: { repository_id: repositoryId } }, true);
  }

  async nativeStatus(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'native_status', body: { repository_id: repositoryId } });
  }

  async launch(
    repositoryId: string,
    mode: Mode,
    readiness: string,
    capsuleId?: string,
  ): Promise<unknown> {
    if (mode === 'verified_local' && (readiness !== 'sealed_ready' || !capsuleId)) {
      throw new Error('Verified local launch requires a SEALED_READY capsule.');
    }
    return await this.invoke({
      command: 'launch',
      body: {
        repository_id: repositoryId,
        capsule_id: capsuleId ?? null,
        maximum_duration_seconds: null,
      },
    });
  }

  async beginUpdate(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'update_begin', body: { repository_id: repositoryId } });
  }

  async updateStatus(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'update_status', body: { repository_id: repositoryId } });
  }

  async commitUpdate(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'update_commit', body: { repository_id: repositoryId } });
  }

  async rollbackUpdate(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'update_rollback', body: { repository_id: repositoryId } });
  }

  async repair(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'repair', body: { repository_id: repositoryId } });
  }

  private async invoke(command: IpcCommand, authenticateDrive = false): Promise<unknown> {
    const request: Request = {
      protocol_version: PROTOCOL_VERSION,
      request_id: ++this.requestId,
      cancellation_id: null,
      command,
    };
    const response = authenticateDrive
      ? await this.invokeDrive(request)
      : await this.bridge.invoke(request);
    if (!record(response) || response.protocol_version !== PROTOCOL_VERSION || response.request_id !== request.request_id || !record(response.body)) {
      throw new ProtocolMismatch('Service protocol version or response correlation mismatch.');
    }
    switch (response.body.kind) {
      case 'json': return response.body.value;
      case 'accepted': return { ...response.body.value, accepted: true };
      case 'progress': return response.body.value;
      case 'error': throw new ServiceError(response.body.value.code, response.body.value.message);
      default: throw new ProtocolMismatch('The service returned an unrecognized result. Refresh before continuing.');
    }
  }

  private async invokeDrive(request: Request): Promise<Response> {
    if (!this.bridge.invokeDrive) {
      throw new Error('This MirageSSD UI host cannot broker the protected Drive session. Reopen the installed app.');
    }
    return await this.bridge.invokeDrive(request);
  }

  private async apiGet(path: string): Promise<unknown> {
    if (!this.bridge.apiGet) {
      throw new Error('This MirageSSD UI host does not expose local setup endpoints. Reopen the installed app.');
    }
    return await this.bridge.apiGet(path);
  }

  private async apiPost(path: string, body?: unknown): Promise<unknown> {
    if (!this.bridge.apiPost) {
      throw new Error('This MirageSSD UI host does not expose local setup endpoints. Reopen the installed app.');
    }
    return await this.bridge.apiPost(path, body);
  }

  async disks(): Promise<DisksPayload> {
    const value = await this.apiGet('/api/disks');
    if (!record(value) || !Array.isArray(value.disks)) {
      throw new ProtocolMismatch('The desktop returned incomplete disk information.');
    }
    return value as unknown as DisksPayload;
  }

  async driveStatus(): Promise<DriveStatus> {
    return await this.apiGet('/api/drive/status') as DriveStatus;
  }

  async driveLogin(): Promise<void> {
    const value = await this.apiPost('/api/drive/login');
    if (record(value) && value.started === false && value.in_flight !== true) {
      throw new Error(typeof value.error === 'string' ? value.error : 'Sign-in could not be started.');
    }
  }

  async serviceStart(): Promise<void> {
    const payload = await this.apiPost('/api/service/start') as { ok?: boolean };
    if (!payload?.ok) throw new Error('Windows did not offer to start the service.');
  }

  async updateCheck(): Promise<UpdateCheck> {
    return await this.apiGet('/api/update/check') as UpdateCheck;
  }

  async pinQuickAccess(letter: string): Promise<void> {
    const payload = await this.apiPost('/api/pin-quick-access', { letter }) as { ok?: boolean; error?: string };
    if (!payload?.ok) throw new Error(payload?.error ?? 'could not pin to Quick Access');
  }

  async diagnosticsCollect(): Promise<string> {
    const payload = await this.apiPost('/api/diagnostics/collect') as { path?: string; error?: string };
    const path = payload?.path;
    if (typeof path === 'string' && path.length > 0) return path;
    throw new Error(typeof payload?.error === 'string' ? payload.error : 'diagnostics failed');
  }

  async driveLoginCancel(): Promise<void> {
    await this.apiPost('/api/drive/login/cancel');
  }

  async driveLogout(): Promise<void> {
    await this.apiPost('/api/drive/logout');
  }

  async volumeCreate(payload: { name: string; letter: string; budget_bytes: number; floor_bytes?: number; cache_disk?: string }): Promise<void> {
    const value = await this.apiPost('/api/volume/create', payload);
    if (record(value) && typeof value.error === 'string') throw new Error(value.error);
  }

  async volumeCreateStatus(): Promise<VolumeCreateStatus> {
    return await this.apiGet('/api/volume/create-status') as VolumeCreateStatus;
  }

  /// Move a volume's local cache to another disk (the host unmounts,
  /// relocates with verification, and remounts); poll volumeSetCacheStatus.
  async volumeSetCache(payload: { repository_id: string; cache_disk?: string }): Promise<void> {
    const value = await this.apiPost('/api/volume/set-cache', payload);
    if (record(value) && typeof value.error === 'string') throw new Error(value.error);
    if (record(value) && value.in_flight === true && value.started !== true) throw new Error('Another cache move is already running.');
  }

  async volumeSetCacheStatus(): Promise<VolumeSetCacheStatus> {
    return await this.apiGet('/api/volume/set-cache-status') as VolumeSetCacheStatus;
  }

  async openExplorer(letter: string): Promise<void> {
    const value = await this.apiPost('/api/open-explorer', { letter });
    if (record(value) && typeof value.error === 'string') throw new Error(value.error);
  }

  async pins(repositoryId: string): Promise<unknown> {
    return await this.invoke({ command: 'namespace_pins', body: { repository_id: repositoryId } });
  }

  async pin(repositoryId: string, path: string): Promise<unknown> {
    return await this.invoke({ command: 'namespace_pin', body: { repository_id: repositoryId, path } });
  }

  async unpin(repositoryId: string, path: string): Promise<unknown> {
    return await this.invoke({ command: 'namespace_unpin', body: { repository_id: repositoryId, path } });
  }

  async diskStatus(): Promise<unknown> {
    return await this.invoke({ command: 'disk_status' });
  }

  async diskReclaimNow(): Promise<unknown> {
    return await this.invoke({ command: 'disk_reclaim_now' });
  }

  /// Set the always-keep-free floor for a disk root such as `D:\`.
  async diskFloorSet(volumeRoot: string, floorBytes: number): Promise<unknown> {
    return await this.invoke({ command: 'disk_floor_set', body: { volume_root: volumeRoot, floor_bytes: floorBytes } });
  }
}

function normalizeRepository(value: unknown) {
  if (!record(value) || typeof value.repository_id !== 'string' || typeof value.display_name !== 'string' || typeof value.state !== 'string'
      || !(value.active_generation === null || (typeof value.active_generation === 'number' && Number.isSafeInteger(value.active_generation) && value.active_generation >= 0))
      || !(value.active_commit === null || typeof value.active_commit === 'string')) {
    throw new ProtocolMismatch('The service returned incomplete drive information. Refresh before continuing.');
  }
  const repository = value as ServiceRepository;
  const size = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
  return {
    id: repository.repository_id,
    name: repository.display_name,
    generation: repository.active_generation,
    commit: repository.active_commit,
    state: repository.state,
    mounted: repository.state === 'ready_mounted',
    physicalBytes: size(repository.physical_bytes),
    logicalBytes: size(repository.logical_bytes),
    backendHealth: repository.backend_health ?? 'unknown',
    lastSealViolations: size(repository.last_seal_violations) ?? 0,
    pendingBytes: size(repository.unpublished_payload_bytes),
    publishedBytes: size(repository.published_payload_bytes),
    pendingOperations: size(repository.pending_local_operations),
    diverged: typeof repository.diverged === 'boolean' ? repository.diverged : null,
    volumeMode: repository.volume_mode,
    origin: repository.origin,
    mountPath: repository.mount_path,
    cacheRoot: repository.cache_root,
    cacheDiskRoot: repository.cache_disk_root,
    cacheDiskFreeBytes: size(repository.cache_disk_free_bytes),
    cacheDiskTotalBytes: size(repository.cache_disk_total_bytes),
    cacheDiskFloorBytes: size(repository.cache_disk_floor_bytes),
  };
}

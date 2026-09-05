import {
  PROTOCOL_VERSION,
  type IpcCommand,
  type CapacityLease,
  type CapacityPlan,
  type Mode,
  type Request,
  type Response,
  type ServiceRepository,
  type ServiceSnapshot,
  type StatusPayload,
} from '../models';

export interface LocalBridge {
  invoke(request: Request): Promise<Response>;
  invokeDrive?(request: Request): Promise<Response>;
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
    });
    if (!response.ok) {
      throw new Error(`The local MirageSSD bridge returned HTTP ${response.status}.`);
    }
    return await response.json() as Response;
  }
}

export class ServiceClient {
  private requestId = 0;

  constructor(private readonly bridge: LocalBridge) {}

  async snapshot(): Promise<ServiceSnapshot> {
    const value = await this.invoke({ command: 'status' }) as StatusPayload;
    return {
      protocolVersion: PROTOCOL_VERSION,
      service: value.service,
      configured: value.configured,
      repositories: value.repositories.map(normalizeRepository),
    };
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
    if (response.protocol_version !== PROTOCOL_VERSION || response.request_id !== request.request_id) {
      throw new ProtocolMismatch('Service protocol version or response correlation mismatch.');
    }
    switch (response.body.kind) {
      case 'json': return response.body.value;
      case 'accepted': return response.body.value;
      case 'progress': return response.body.value;
      case 'error': throw new ServiceError(response.body.value.code, response.body.value.message);
    }
  }

  private async invokeDrive(request: Request): Promise<Response> {
    if (!this.bridge.invokeDrive) {
      throw new Error('This MirageSSD UI host cannot broker the protected Drive session. Reopen the installed app.');
    }
    return await this.bridge.invokeDrive(request);
  }
}

function normalizeRepository(repository: ServiceRepository) {
  return {
    id: repository.repository_id,
    name: repository.display_name,
    generation: repository.active_generation,
    commit: repository.active_commit,
    state: repository.state,
    mounted: repository.state === 'ready_mounted',
    physicalBytes: repository.physical_bytes ?? null,
    logicalBytes: repository.logical_bytes ?? null,
    backendHealth: repository.backend_health ?? 'not_configured',
    lastSealViolations: repository.last_seal_violations ?? 0,
  };
}

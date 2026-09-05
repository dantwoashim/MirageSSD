export const PROTOCOL_VERSION = 3 as const;

export type Mode = 'verified_local';
export type ReadinessState = 'not_ready' | 'materializing' | 'sealed_ready';

export type RepositoryState = {
  id: string;
  name: string;
  generation: number | null;
  commit: string | null;
  state: string;
  mounted: boolean;
  physicalBytes: number | null;
  logicalBytes: number | null;
  backendHealth: string;
  lastSealViolations: number;
};

export type Readiness = {
  state: ReadinessState;
  capsuleId?: string;
  hardSetBytes: number;
  envelopeBytes: number;
  scanMapBytes: number;
  frontierBytes: number;
  updateReserveBytes: number;
  missingBytes: number;
  heldOutViolations: number;
  lastSealViolation?: string;
};

export type ServiceSnapshot = {
  protocolVersion: number;
  service: string;
  configured: boolean;
  repositories: RepositoryState[];
};

export type ServiceRepository = {
  repository_id: string;
  display_name: string;
  state: string;
  active_generation: number | null;
  active_commit: string | null;
  physical_bytes?: number;
  logical_bytes?: number;
  backend_health?: string;
  last_seal_violations?: number;
};

export type StatusPayload = {
  service: string;
  configured: boolean;
  repositories: ServiceRepository[];
};

export type CapacityPlan = {
  repository_id: string;
  origin: 'local' | 'drive';
  drive_authenticated: boolean;
  target_volume_id: string;
  physical_total_bytes: number;
  physical_free_bytes: number;
  physical_total_free_bytes: number;
  filesystem_reserve_bytes: number;
  requested_bytes: number;
  immediately_available_bytes: number;
  reclaim_required_bytes: number;
  total_reclaimable_bytes: number;
  selected_reclaim_bytes: number;
  selected_reclaim_unit_count: number;
  selected_cache_page_count: number;
  selected_drive_shadow_pack_count: number;
  selected_native_backup_count: number;
  available_after_selected_reclaim_bytes: number;
  shortfall_bytes: number;
  grantable: boolean;
  remote_quota_counted_as_local_capacity: false;
  blocked: {
    unique_bytes: number;
    unverified_bytes: number;
    dirty_bytes: number;
    pinned_bytes: number;
    active_read_bytes: number;
  };
};

export type CapacityLease = {
  repository_id: string;
  lease_id: string;
  state: string;
  target_volume_id: string;
  requested_bytes: number;
  physical_free_after_reclaim_bytes: number;
  filesystem_reserve_bytes: number;
  active_promised_bytes: number;
  expires_at_ns: number;
  ready: boolean;
  remote_quota_counted_as_local_capacity: false;
};

export type IpcCommand =
  | { command: 'status' }
  | { command: 'repository_list' }
  | { command: 'repository_detail'; body: { repository_id: string } }
  | { command: 'mount'; body: { repository_id: string; generation: number } }
  | { command: 'unmount'; body: { repository_id: string } }
  | { command: 'profile'; body: { repository_id: string; maximum_duration_seconds: number } }
  | { command: 'simulate'; body: { repository_id: string } }
  | { command: 'capacity_plan'; body: { repository_id: string; requested_bytes: number } }
  | { command: 'capacity_acquire'; body: { repository_id: string; requested_bytes: number; lifetime_seconds: number } }
  | { command: 'capacity_status'; body: { repository_id: string; lease_id?: string } }
  | { command: 'capacity_consume'; body: { repository_id: string; lease_id: string } }
  | { command: 'capacity_release'; body: { repository_id: string; lease_id: string } }
  | { command: 'native_activate'; body: { repository_id: string } }
  | { command: 'native_status'; body: { repository_id: string } }
  | { command: 'plan'; body: { repository_id: string; full_volume: boolean } }
  | { command: 'materialize'; body: { repository_id: string; capsule_id: string } }
  | { command: 'admit'; body: { repository_id: string; capsule_id: string } }
  | { command: 'launch'; body: { repository_id: string; capsule_id: string | null; maximum_duration_seconds: number | null } }
  | { command: 'verify'; body: { repository_id: string; deep: boolean } }
  | { command: 'update_begin'; body: { repository_id: string } }
  | { command: 'update_status'; body: { repository_id: string } }
  | { command: 'update_commit'; body: { repository_id: string } }
  | { command: 'update_rollback'; body: { repository_id: string } }
  | { command: 'repair'; body: { repository_id: string } };

export type Request = {
  protocol_version: typeof PROTOCOL_VERSION;
  request_id: number;
  cancellation_id: number | null;
  command: IpcCommand;
};

export type ResponseBody =
  | { kind: 'json'; value: unknown }
  | { kind: 'accepted'; value: { operation_id: number } }
  | { kind: 'progress'; value: { completed: number; total: number } }
  | { kind: 'error'; value: { code: string; message: string } };

export type Response = {
  protocol_version: number;
  request_id: number;
  body: ResponseBody;
};

export const EMPTY_READINESS: Readiness = {
  state: 'not_ready',
  hardSetBytes: 0,
  envelopeBytes: 0,
  scanMapBytes: 0,
  frontierBytes: 0,
  updateReserveBytes: 0,
  missingBytes: 0,
  heldOutViolations: 0,
};

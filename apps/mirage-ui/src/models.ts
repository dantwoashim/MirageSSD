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
  pendingBytes?: number | null;
  publishedBytes?: number | null;
  pendingOperations?: number | null;
  diverged?: boolean | null;
  volumeMode?: string;
  origin?: string;
  mountPath?: string;
  cacheRoot?: string;
  cacheDiskRoot?: string;
  cacheDiskFreeBytes?: number | null;
  cacheDiskTotalBytes?: number | null;
  cacheDiskFloorBytes?: number | null;
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

export type DiskInfo = {
  volume_root: string;
  total_bytes: number;
  free_bytes: number;
};

export type DisksPayload = {
  disks: DiskInfo[];
  state_volume: DiskInfo | null;
  default_letter: string;
  default_budget_bytes: number;
  free_letters?: string[];
  /// Disk root that will hold the local cache unless the user picks another.
  default_cache_disk?: string | null;
};

export type DriveStatus = {
  authenticated: boolean;
  account_id: string | null;
  issued_unix_seconds: number | null;
  login: 'idle' | 'in_flight' | { done: string } | { failed: string };
};

export type UpdateCheck = {
  ok?: boolean;
  checked?: boolean;
  checked_at?: number;
  current?: string;
  channel?: string;
  latest?: string;
  url?: string;
  update_available?: boolean;
};

export type VolumeCreateStatus = {
  in_flight?: boolean;
  step?: string;
  done?: boolean;
  error?: string;
  repository_id?: string;
  drive_letter?: string;
  name?: string;
  budget_bytes?: number;
  account_id?: string;
  cache_root?: string | null;
};

export type VolumeOffloadStatus = {
  in_flight?: boolean;
  step?: string;
  done?: boolean;
  error?: string;
  repository_id?: string;
  source?: string;
  destination?: string;
  files?: number;
  bytes?: number;
  copied?: number;
  skipped_identical?: number;
  verified?: number;
  published?: boolean;
  unpublished_bytes_remaining?: number;
  source_deleted?: boolean;
  failures?: string[];
};

export type VolumeSetCacheStatus = {
  in_flight?: boolean;
  step?: string;
  done?: boolean;
  error?: string;
  repository_id?: string;
  cache_root?: string;
  cache_disk_root?: string | null;
  moved_payloads?: number;
  moved_bytes?: number;
  remounted?: boolean;
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
  unpublished_payload_bytes?: number;
  published_payload_bytes?: number;
  pending_local_operations?: number;
  diverged?: boolean;
  volume_mode?: string;
  origin?: string;
  mount_path?: string;
  cache_root?: string;
  cache_disk_root?: string;
  cache_disk_free_bytes?: number;
  cache_disk_total_bytes?: number;
  cache_disk_floor_bytes?: number;
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
  | { command: 'repository_unregister'; body: { repository_id: string; force_unmount: boolean; discard_unpublished: boolean } }
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
  | { command: 'repair'; body: { repository_id: string } }
  | { command: 'namespace_pin'; body: { repository_id: string; path: string } }
  | { command: 'namespace_unpin'; body: { repository_id: string; path: string } }
  | { command: 'namespace_pins'; body: { repository_id: string } }
  | { command: 'disk_status' }
  | { command: 'disk_floor_set'; body: { volume_root: string; floor_bytes: number } }
  | { command: 'disk_reclaim_now' };

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

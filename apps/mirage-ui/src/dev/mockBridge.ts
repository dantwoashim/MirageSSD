// Dev-only mock service for the demo harness — never part of the production
// bundle. Implements LocalBridge with plausible, non-round data so every
// screen state can be exercised without the Windows service.
import type { LocalBridge } from '../api/client';
import { REPO_URL } from '../links';
import type { Request, Response, ServiceRepository } from '../models';

export type DemoScenario = 'fresh' | 'drive' | 'uploading' | 'multi' | 'offline';

const GIB = 2 ** 30;
const gib = (n: number) => Math.round(n * GIB);
const OFFLINE_MESSAGE = 'MirageSSD is not responding. Open the desktop app, then reconnect.';
const ACCOUNT = 'maya.k@example.com';

function managedDrive(overrides: Partial<ServiceRepository>): ServiceRepository {
  return {
    repository_id: '7f3a9c1e'.repeat(4),
    display_name: 'MirageSSD',
    state: 'ready_mounted',
    active_generation: 0,
    active_commit: 'b2'.repeat(32),
    physical_bytes: gib(18.4),
    logical_bytes: gib(212.6),
    backend_health: 'healthy',
    last_seal_violations: 0,
    unpublished_payload_bytes: 0,
    published_payload_bytes: gib(212.6),
    pending_local_operations: 0,
    volume_mode: 'managed',
    origin: 'drive',
    mount_path: 'M:\\',
    cache_root: 'C:\\Users\\maya\\AppData\\Local\\MirageSSD\\cache\\7f3a9c1e',
    cache_disk_root: 'C:\\',
    cache_disk_free_bytes: gib(143.2),
    cache_disk_total_bytes: gib(476.3),
    cache_disk_floor_bytes: gib(47),
    ...overrides,
  };
}

function engineRepository(): ServiceRepository {
  return {
    repository_id: 'e5d1b8f4'.repeat(4),
    display_name: 'Steam library (import)',
    state: 'ready_unmounted',
    active_generation: 3,
    active_commit: '9d'.repeat(32),
    physical_bytes: gib(41.7),
    logical_bytes: gib(96.2),
    backend_health: 'online',
    last_seal_violations: 0,
    unpublished_payload_bytes: gib(0.6),
    published_payload_bytes: gib(95.6),
    pending_local_operations: 0,
    volume_mode: 'engine',
    origin: 'local',
  };
}

export function mockBridge(scenario: DemoScenario): LocalBridge {
  if (scenario === 'offline') {
    const fail = async (): Promise<never> => {
      throw new Error(OFFLINE_MESSAGE);
    };
    return { invoke: fail, invokeDrive: fail, apiGet: fail, apiPost: fail };
  }

  const fresh = scenario === 'fresh';
  const repositories: ServiceRepository[] = [];
  if (!fresh) {
    repositories.push(
      managedDrive({
        unpublished_payload_bytes: scenario === 'uploading' ? gib(1.37) : 0,
      }),
    );
    if (scenario === 'multi') {
      repositories.push(
        managedDrive({
          repository_id: '4c8e2a6d'.repeat(4),
          display_name: 'Projects',
          state: 'ready_unmounted',
          mount_path: 'N:\\',
          cache_root: 'D:\\MirageSSD\\cache\\4c8e2a6d',
          cache_disk_root: 'D:\\',
          physical_bytes: gib(2.1),
          logical_bytes: gib(87.4),
          published_payload_bytes: gib(87.4),
        }),
        engineRepository(),
      );
    }
  }

  const pins = new Map<string, string[]>([
    ['7f3a9c1e'.repeat(4), ['/Photos', '/Work/Current']],
  ]);
  const floors = new Map<string, number>([['C:\\', gib(47)]]);
  let reclaimedAvailable = gib(2.3);

  let authenticated = !fresh;
  let loginInFlight = false;
  let loginPolls = 0;
  let createInFlight = false;
  let createPolls = 0;
  let createdName = 'MirageSSD';
  let createdLetter = 'M';
  let offloadInFlight = false;
  let offloadPolls = 0;
  let offloadDeleteSource = false;
  let setCacheInFlight = false;
  let setCachePolls = 0;
  let setCacheTarget = 'D:\\';

  const reply = (request: Request, value: unknown): Response => ({
    protocol_version: 3,
    request_id: request.request_id,
    body: { kind: 'json', value },
  });

  const findRepository = (id: string) =>
    repositories.find((repository) => repository.repository_id === id) ?? repositories[0];

  const invoke = async (request: Request): Promise<Response> => {
    const command = request.command;
    switch (command.command) {
      case 'status':
        return reply(request, {
          service: 'running',
          configured: authenticated && repositories.length > 0,
          repositories,
        });
      case 'repository_detail':
        return reply(request, findRepository(command.body.repository_id));
      case 'mount': {
        const repository = findRepository(command.body.repository_id);
        repository.state = 'ready_mounted';
        repository.mount_path ??= 'M:\\';
        return reply(request, {});
      }
      case 'unmount': {
        findRepository(command.body.repository_id).state = 'ready_unmounted';
        return reply(request, {});
      }
      case 'repository_unregister': {
        const index = repositories.findIndex((repository) => repository.repository_id === command.body.repository_id);
        if (index >= 0) repositories.splice(index, 1);
        return reply(request, {});
      }
      case 'namespace_pins':
        return reply(request, { pins: (pins.get(command.body.repository_id) ?? []).map((path) => ({ path })) });
      case 'namespace_pin': {
        const list = pins.get(command.body.repository_id) ?? [];
        if (!list.includes(command.body.path)) list.push(command.body.path);
        pins.set(command.body.repository_id, list);
        return reply(request, {});
      }
      case 'namespace_unpin': {
        pins.set(
          command.body.repository_id,
          (pins.get(command.body.repository_id) ?? []).filter((path) => path !== command.body.path),
        );
        return reply(request, {});
      }
      case 'disk_status':
        return reply(request, { floors: [...floors].map(([volume_root, floor_bytes]) => ({ volume_root, floor_bytes })) });
      case 'disk_floor_set':
        floors.set(command.body.volume_root, command.body.floor_bytes);
        for (const repository of repositories) {
          if (repository.cache_disk_root === command.body.volume_root) {
            repository.cache_disk_floor_bytes = command.body.floor_bytes;
          }
        }
        return reply(request, {});
      case 'disk_reclaim_now': {
        const freed = reclaimedAvailable;
        reclaimedAvailable = 0;
        for (const repository of repositories) {
          repository.physical_bytes = Math.max(0, (repository.physical_bytes ?? 0) - freed);
        }
        return reply(request, { reclaimed_bytes: freed });
      }
      case 'capacity_plan':
        return reply(request, {
          repository_id: command.body.repository_id,
          origin: 'drive',
          drive_authenticated: true,
          target_volume_id: 'C:\\',
          physical_total_bytes: gib(476.3),
          physical_free_bytes: gib(143.2),
          physical_total_free_bytes: gib(143.2),
          filesystem_reserve_bytes: gib(47),
          requested_bytes: command.body.requested_bytes,
          immediately_available_bytes: gib(96.2),
          reclaim_required_bytes: Math.max(0, command.body.requested_bytes - gib(96.2)),
          total_reclaimable_bytes: gib(18.4),
          selected_reclaim_bytes: gib(18.4),
          selected_reclaim_unit_count: 3,
          selected_cache_page_count: 4217,
          selected_drive_shadow_pack_count: 0,
          selected_native_backup_count: 0,
          available_after_selected_reclaim_bytes: gib(114.6),
          shortfall_bytes: 0,
          grantable: true,
          remote_quota_counted_as_local_capacity: false,
          blocked: {
            unique_bytes: gib(1.1),
            unverified_bytes: 0,
            dirty_bytes: gib(0.4),
            pinned_bytes: gib(6.8),
            active_read_bytes: 0,
          },
        });
      case 'capacity_acquire':
        return reply(request, {
          repository_id: command.body.repository_id,
          lease_id: `lease-${Date.now().toString(16)}`,
          state: 'active',
          target_volume_id: 'C:\\',
          requested_bytes: command.body.requested_bytes,
          physical_free_after_reclaim_bytes: gib(114.6),
          filesystem_reserve_bytes: gib(47),
          active_promised_bytes: command.body.requested_bytes,
          expires_at_ns: Date.now() * 1e6 + 21_600 * 1e9,
          ready: true,
          remote_quota_counted_as_local_capacity: false,
        });
      case 'capacity_status':
        return reply(request, { leases: [] });
      case 'capacity_consume':
      case 'capacity_release':
        return reply(request, {});
      case 'native_status':
        return reply(request, { state: 'not_started' });
      case 'native_activate':
        return reply(request, { state: 'prepared' });
      case 'plan':
        return reply(request, {
          capsule_id: 'aa'.repeat(32),
          state: 'materializing',
          total_bytes: gib(41.7),
          missing_bytes: gib(4.3),
          hard_set_bytes: gib(12.6),
          envelope_bytes: gib(28.9),
          scan_map_bytes: gib(2.2),
          frontier_bytes: gib(3.1),
          update_reserve_bytes: gib(4.4),
          held_out_violations: 0,
        });
      case 'materialize':
        return reply(request, { complete: true });
      case 'admit':
        return reply(request, { state: 'sealed_ready' });
      case 'launch':
        return reply(request, { launched: true });
      case 'verify':
        return reply(request, { verified: true });
      case 'update_begin':
        return reply(request, { state: 'journal_open' });
      case 'update_status':
        return reply(request, { state: 'sealed', journal_clean: true });
      case 'update_commit':
        return reply(request, { state: 'committed' });
      case 'update_rollback':
        return reply(request, { state: 'rolled_back' });
      case 'repair':
        return reply(request, { repaired: true });
      case 'profile':
      case 'simulate':
      case 'repository_list':
        return reply(request, { repositories });
      default:
        return reply(request, {});
    }
  };

  const apiGet = async (path: string): Promise<unknown> => {
    switch (path) {
      case '/api/drive/status':
        if (loginInFlight && ++loginPolls >= 3) {
          loginInFlight = false;
          authenticated = true;
        }
        return {
          authenticated,
          account_id: authenticated ? ACCOUNT : null,
          issued_unix_seconds: authenticated ? Math.floor(Date.now() / 1000) - 420 : null,
          login: loginInFlight ? 'in_flight' : 'idle',
        };
      case '/api/disks':
        return {
          disks: [
            { volume_root: 'C:\\', total_bytes: gib(476.3), free_bytes: gib(143.2) },
            { volume_root: 'D:\\', total_bytes: gib(931.5), free_bytes: gib(412.8) },
          ],
          state_volume: { volume_root: 'C:\\', total_bytes: gib(476.3), free_bytes: gib(143.2) },
          default_letter: scenario === 'multi' ? 'O' : 'M',
          default_budget_bytes: gib(24),
          free_letters: scenario === 'multi' ? ['O', 'P', 'Q'] : ['M', 'N', 'O', 'P'],
          default_cache_disk: 'C:\\',
        };
      case '/api/update/check':
        return scenario === 'multi'
          ? {
              checked: true,
              checked_at: Math.floor(Date.now() / 1000) - 5400,
              current: '0.1.17',
              channel: 'preview',
              latest: 'v0.1.18',
              url: `${REPO_URL}/releases/tag/v0.1.18`,
              update_available: true,
            }
          : {
              checked: true,
              checked_at: Math.floor(Date.now() / 1000) - 5400,
              current: '0.1.17',
              channel: 'preview',
              update_available: false,
            };
      case '/api/volume/create-status': {
        if (!createInFlight && createPolls === 0) return {};
        createPolls += 1;
        if (createPolls >= 3) {
          createInFlight = false;
          const repository = managedDrive({
            display_name: createdName,
            mount_path: `${createdLetter}:\\`,
            cache_disk_root: 'C:\\',
          });
          if (!repositories.some((item) => item.repository_id === repository.repository_id)) {
            repositories.push(repository);
          }
          return {
            in_flight: false,
            done: true,
            repository_id: repository.repository_id,
            drive_letter: createdLetter,
            name: createdName,
            budget_bytes: gib(24),
            cache_root: repository.cache_root,
            account_id: ACCOUNT,
          };
        }
        return { in_flight: true, step: createPolls === 1 ? 'Creating the drive' : 'Connecting to Google Drive' };
      }
      case '/api/volume/offload-status': {
        if (!offloadInFlight && offloadPolls === 0) return {};
        offloadPolls += 1;
        if (offloadPolls >= 3) {
          offloadInFlight = false;
          return {
            in_flight: false,
            done: true,
            repository_id: repositories[0]?.repository_id,
            files: 428,
            bytes: gib(1.37),
            copied: 428,
            skipped_identical: 0,
            verified: 428,
            published: true,
            unpublished_bytes_remaining: 0,
            source_deleted: offloadDeleteSource,
            failures: [],
          };
        }
        return {
          in_flight: true,
          step: ['Copying files into the drive', 'Verifying files through the drive'][offloadPolls - 1] ?? 'Waiting for Google Drive',
          files: 428,
          copied: offloadPolls * 137,
        };
      }
      case '/api/volume/set-cache-status': {
        if (!setCacheInFlight && setCachePolls === 0) return {};
        setCachePolls += 1;
        if (setCachePolls >= 3) {
          setCacheInFlight = false;
          for (const repository of repositories) {
            if (repository.origin === 'drive') repository.cache_disk_root = setCacheTarget;
          }
          return {
            in_flight: false,
            done: true,
            repository_id: repositories[0]?.repository_id,
            cache_disk_root: setCacheTarget,
            moved_payloads: 312,
            moved_bytes: gib(18.4),
            remounted: true,
          };
        }
        return {
          in_flight: true,
          step: ['Disconnecting the drive', 'Moving the local cache'][setCachePolls - 1] ?? 'Reconnecting the drive',
        };
      }
      default:
        return {};
    }
  };

  const apiPost = async (path: string, body?: unknown): Promise<unknown> => {
    switch (path) {
      case '/api/drive/login':
        loginInFlight = true;
        loginPolls = 0;
        return { started: true };
      case '/api/drive/login/cancel':
        loginInFlight = false;
        return { cancelled: true };
      case '/api/drive/logout':
        authenticated = false;
        return { signed_out: true };
      case '/api/volume/create': {
        const payload = (body ?? {}) as { name?: string; letter?: string };
        createdName = payload.name ?? 'MirageSSD';
        createdLetter = payload.letter ?? 'M';
        createInFlight = true;
        createPolls = 0;
        return { started: true };
      }
      case '/api/volume/offload': {
        offloadInFlight = true;
        offloadPolls = 0;
        offloadDeleteSource = Boolean((body as { delete_source?: boolean })?.delete_source);
        return { started: true };
      }
      case '/api/volume/set-cache': {
        setCacheInFlight = true;
        setCachePolls = 0;
        setCacheTarget = (body as { cache_disk?: string })?.cache_disk ?? 'D:\\';
        return { started: true };
      }
      case '/api/open-explorer':
        return { opened: true };
      case '/api/pin-quick-access':
        return { ok: true };
      case '/api/diagnostics/collect':
        return {
          ok: true,
          path: 'C:\\Users\\maya\\AppData\\Local\\MirageSSD\\diagnostics\\mirage-diagnostics-20261006-1412.zip',
        };
      case '/api/service/start':
        return { ok: true };
      default:
        return {};
    }
  };

  return { invoke, invokeDrive: invoke, apiGet, apiPost };
}

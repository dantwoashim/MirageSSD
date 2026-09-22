import { EMPTY_READINESS, type Mode, type Readiness, type RepositoryState } from './models';

export function canLaunch(mode: Mode, readiness: Readiness) {
  return mode === 'verified_local' && readiness.state === 'sealed_ready' && readiness.missingBytes === 0 && Boolean(readiness.capsuleId);
}

export function validateBudget(bytes: number, mandatory: number, free: number): string | undefined {
  if (![bytes, mandatory, free].every((value) => Number.isSafeInteger(value) && value >= 0)) return 'Enter a valid storage amount.';
  if (bytes < mandatory) return 'Below the required local storage.';
  if (bytes > free) return 'Exceeds available local storage.';
  return undefined;
}

export function repositoryScope(repository?: RepositoryState): string {
  return repository ? `${repository.id}:${repository.generation ?? ''}:${repository.commit ?? ''}` : '';
}

export function record(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function readinessFromPlan(value: unknown): Readiness {
  if (!record(value) || typeof value.capsule_id !== 'string' || !value.capsule_id) {
    throw new Error('The preparation plan is incomplete. Refresh the drive and check again.');
  }
  const metric = (name: string, fallback?: number): number => {
    const result = value[name] ?? fallback;
    if (typeof result !== 'number' || !Number.isSafeInteger(result) || result < 0) {
      throw new Error('The preparation plan contains an invalid size. No offline access has been approved.');
    }
    return result;
  };
  const total = metric('total_bytes');
  const missing = metric('missing_bytes', total);
  return {
    ...EMPTY_READINESS,
    capsuleId: value.capsule_id,
    state: value.state === 'sealed_ready' && missing === 0 ? 'sealed_ready' : missing === 0 ? 'materializing' : 'not_ready',
    hardSetBytes: metric('hard_set_bytes', total),
    envelopeBytes: metric('envelope_bytes', 0),
    scanMapBytes: metric('scan_map_bytes', 0),
    frontierBytes: metric('frontier_bytes', 0),
    updateReserveBytes: metric('update_reserve_bytes', 0),
    missingBytes: missing,
    heldOutViolations: metric('held_out_violations', 0),
  };
}

export function operationMessage(label: string, result: unknown): string {
  if (record(result) && result.accepted === true) return `${label} has started. Its completion is not yet confirmed.`;
  if (record(result) && typeof result.completed === 'number' && typeof result.total === 'number') {
    return `${label}: ${result.completed} of ${result.total} complete.`;
  }
  return `${label} completed.`;
}

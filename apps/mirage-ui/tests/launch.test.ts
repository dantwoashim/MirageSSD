import { expect, it } from 'vitest'; import { canLaunch } from '../src/views/Launch'; import { validateBudget } from '../src/components/CacheBudget';
const readiness = { state: 'not_ready' as const, hardSetBytes: 1, envelopeBytes: 1, scanMapBytes: 1, frontierBytes: 1, updateReserveBytes: 1, missingBytes: 1, heldOutViolations: 1 };
it('fails closed for unready launches and invalid budgets', () => { expect(canLaunch('verified_local', readiness)).toBe(false); expect(validateBudget(5, 10, 100)).toBeTruthy(); expect(validateBudget(50, 10, 40)).toBeTruthy(); });

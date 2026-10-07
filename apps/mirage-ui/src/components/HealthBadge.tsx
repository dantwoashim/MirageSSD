import { CheckCircle, WarningCircle, XCircle } from '@phosphor-icons/react';
import { label } from '../presentation';
export { label } from '../presentation';

const healthy = new Set(['healthy', 'ready', 'running', 'ready_mounted', 'ready_unmounted']);
const warning = new Set(['not_configured', 'degraded', 'recovering', 'updating', 'materializing']);

export function HealthBadge({ value }: { value: string }) {
  const normalized = value.toLowerCase();
  const good = healthy.has(normalized);
  const caution = warning.has(normalized);
  const Icon = good ? CheckCircle : caution ? WarningCircle : XCircle;
  const colors = good
    ? 'border-ok/25 bg-ok-soft text-ok'
    : caution
      ? 'border-warn/25 bg-warn-soft text-warn'
      : 'border-danger/25 bg-danger-soft text-danger';
  return (
    <span
      role="status"
      aria-label={`Health: ${label(value)}`}
      className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-medium ${colors}`}
    >
      <Icon size={14} weight="bold" aria-hidden="true" />
      {label(value)}
    </span>
  );
}

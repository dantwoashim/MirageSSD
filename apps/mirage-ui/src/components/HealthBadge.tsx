import { CheckCircle, WarningCircle, XCircle } from '@phosphor-icons/react';

const healthy = new Set(['healthy', 'ready', 'running', 'ready_mounted', 'ready_unmounted']);
const warning = new Set(['not_configured', 'degraded', 'recovering', 'updating', 'materializing']);

export function HealthBadge({ value }: { value: string }) {
  const normalized = value.toLowerCase();
  const good = healthy.has(normalized);
  const caution = warning.has(normalized);
  const Icon = good ? CheckCircle : caution ? WarningCircle : XCircle;
  const colors = good
    ? 'border-emerald-400/20 bg-emerald-400/8 text-emerald-200'
    : caution
      ? 'border-amber-300/20 bg-amber-300/8 text-amber-100'
      : 'border-rose-300/20 bg-rose-300/8 text-rose-100';
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

export function label(value: string) {
  return value.replaceAll('_', ' ').replace(/\b\w/g, (character) => character.toUpperCase());
}

import type { StatusTone } from '../presentation';
import { cx } from './cx';

const tones: Record<StatusTone, string> = {
  ok: 'bg-ok-soft text-ok',
  warn: 'bg-warn-soft text-warn',
  danger: 'bg-danger-soft text-danger',
  busy: 'bg-accent-soft text-accent',
  neutral: 'bg-surface-2 text-fg-muted',
};

export function StatusPill({ tone = 'neutral', label, className }: { tone?: StatusTone; label: string; className?: string }) {
  return (
    <span role="status" className={cx('inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-xs font-medium', tones[tone], className)}>
      <span className={tone === 'busy' ? 'busy-dot' : 'status-dot'} aria-hidden="true" />
      {label}
    </span>
  );
}

import { CheckCircle, Info, Warning, WarningCircle, X } from '@phosphor-icons/react';
import type { ReactNode } from 'react';
import { cx } from './cx';

export type NoticeTone = 'info' | 'success' | 'warning' | 'danger';

const tones: Record<NoticeTone, { box: string; icon: typeof Info }> = {
  info: { box: 'border-accent/25 bg-accent-soft text-fg', icon: Info },
  success: { box: 'border-ok/25 bg-ok-soft text-fg', icon: CheckCircle },
  warning: { box: 'border-warn/25 bg-warn-soft text-fg', icon: Warning },
  danger: { box: 'border-danger/25 bg-danger-soft text-fg', icon: WarningCircle },
};
const iconTones: Record<NoticeTone, string> = {
  info: 'text-accent',
  success: 'text-ok',
  warning: 'text-warn',
  danger: 'text-danger',
};

export function Notice({
  tone = 'info',
  title,
  children,
  action,
  onDismiss,
  className,
}: {
  tone?: NoticeTone;
  title?: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
  onDismiss?: () => void;
  className?: string;
}) {
  const Icon = tones[tone].icon;
  return (
    <div
      role={tone === 'warning' || tone === 'danger' ? 'alert' : 'status'}
      className={cx('flex items-start gap-3 rounded-xl border p-3.5 text-[13px] leading-relaxed', tones[tone].box, className)}
    >
      <Icon size={18} weight="bold" className={cx('mt-0.5 shrink-0', iconTones[tone])} aria-hidden="true" />
      <div className="min-w-0 flex-1">
        {title && <p className="font-semibold">{title}</p>}
        {children && <div className="text-fg-muted">{children}</div>}
      </div>
      {action}
      {onDismiss && (
        <button
          type="button"
          aria-label="Dismiss"
          onClick={onDismiss}
          className="shrink-0 rounded-md p-1 text-fg-muted transition-colors duration-150 ease-standard hover:bg-surface-2 hover:text-fg"
        >
          <X size={16} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}

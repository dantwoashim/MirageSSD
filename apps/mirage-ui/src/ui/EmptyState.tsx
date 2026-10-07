import type { ReactNode } from 'react';
import { cx } from './cx';

export function EmptyState({
  icon,
  title,
  body,
  actions,
  className,
}: {
  icon?: ReactNode;
  title: ReactNode;
  body?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <section className={cx('rounded-2xl border border-line bg-surface px-6 py-12 md:px-10', className)}>
      <div className="max-w-xl">
        {icon && (
          <span className="grid size-12 place-items-center rounded-xl border border-line bg-accent-soft text-accent" aria-hidden="true">
            {icon}
          </span>
        )}
        <h2 className="mt-5 font-display text-xl font-semibold tracking-[-0.02em]">{title}</h2>
        {body && <div className="mt-2.5 max-w-[60ch] text-[13px] leading-relaxed text-fg-muted">{body}</div>}
        {actions && <div className="mt-6 flex flex-wrap items-center gap-2.5">{actions}</div>}
      </div>
    </section>
  );
}

import type { HTMLAttributes, ReactNode } from 'react';
import { cx } from './cx';

export function Card({
  padding = 'md',
  title,
  description,
  actions,
  className,
  children,
  ...rest
}: HTMLAttributes<HTMLElement> & {
  padding?: 'md' | 'lg';
  title?: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <section
      className={cx('rounded-2xl border border-line bg-surface', padding === 'lg' ? 'p-6 md:p-7' : 'p-4 md:p-5', className)}
      {...rest}
    >
      {(title || description || actions) && (
        <div className={cx('flex items-start justify-between gap-4', children ? 'mb-4' : '')}>
          <div className="min-w-0">
            {title && <h2 className="section-title">{title}</h2>}
            {description && <div className="mt-1 text-[13px] leading-relaxed text-fg-muted">{description}</div>}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </div>
      )}
      {children}
    </section>
  );
}

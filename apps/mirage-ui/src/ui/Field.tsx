import type { InputHTMLAttributes, ReactNode, SelectHTMLAttributes } from 'react';
import { cx } from './cx';

export function Field({
  label,
  hint,
  error,
  htmlFor,
  children,
  className,
}: {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cx('grid gap-1.5', className)}>
      {label && (
        <label htmlFor={htmlFor} className="field-label">
          {label}
        </label>
      )}
      {children}
      {error ? (
        <p role="alert" className="text-xs leading-relaxed text-danger">
          {error}
        </p>
      ) : hint ? (
        <p className="text-xs leading-relaxed text-fg-subtle">{hint}</p>
      ) : null}
    </div>
  );
}

const controlStyles =
  'min-h-9 w-full rounded-lg border border-line bg-surface-2 px-3 text-[13px] text-fg placeholder:text-fg-subtle transition-colors duration-150 ease-standard hover:border-line-strong disabled:opacity-50';

export function Input({ className, ...rest }: InputHTMLAttributes<HTMLInputElement>) {
  return <input className={cx(controlStyles, className)} {...rest} />;
}

export function Select({ className, children, ...rest }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select className={cx(controlStyles, 'pr-8', className)} {...rest}>
      {children}
    </select>
  );
}

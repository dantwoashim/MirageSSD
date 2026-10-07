import type { AnchorHTMLAttributes, ButtonHTMLAttributes, ReactNode } from 'react';
import { cx } from './cx';
import { Spinner } from './Spinner';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger';
export type ButtonSize = 'md' | 'sm';

const base =
  'inline-flex items-center justify-center gap-2 rounded-lg font-medium transition-colors duration-150 ease-standard active:scale-[0.98] disabled:opacity-50 disabled:pointer-events-none';
const variants: Record<ButtonVariant, string> = {
  primary: 'bg-accent text-on-accent hover:bg-accent/90',
  secondary: 'border border-line-strong bg-surface-2 text-fg hover:bg-line',
  ghost: 'text-fg-muted hover:bg-surface-2 hover:text-fg',
  danger: 'bg-danger-soft text-danger hover:bg-danger/20',
};
const sizes: Record<ButtonSize, string> = {
  md: 'min-h-9 px-4 text-[13px]',
  sm: 'min-h-[30px] px-3 text-[12.5px]',
};

export function buttonStyles({ variant = 'primary', size = 'md' }: { variant?: ButtonVariant; size?: ButtonSize } = {}): string {
  return cx(base, variants[variant], sizes[size]);
}

export function Button({
  variant = 'primary',
  size = 'md',
  icon,
  loading = false,
  className,
  children,
  type = 'button',
  disabled,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: ReactNode;
  loading?: boolean;
}) {
  return (
    <button type={type} disabled={disabled || loading} className={cx(buttonStyles({ variant, size }), className)} {...rest}>
      {loading ? <Spinner size={15} /> : icon}
      {children}
    </button>
  );
}

export function ButtonLink({
  variant = 'primary',
  size = 'md',
  icon,
  className,
  children,
  href,
  external,
  ...rest
}: AnchorHTMLAttributes<HTMLAnchorElement> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: ReactNode;
  external?: boolean;
}) {
  const isExternal = external ?? /^https?:\/\//.test(href ?? '');
  return (
    <a
      href={href}
      className={cx(buttonStyles({ variant, size }), className)}
      {...(isExternal ? { target: '_blank', rel: 'noreferrer' } : {})}
      {...rest}
    >
      {icon}
      {children}
    </a>
  );
}

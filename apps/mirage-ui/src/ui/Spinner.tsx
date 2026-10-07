import { SpinnerGap } from '@phosphor-icons/react';
import { cx } from './cx';

export function Spinner({ size = 16, className }: { size?: number; className?: string }) {
  return <SpinnerGap size={size} className={cx('animate-spin', className)} aria-hidden="true" />;
}

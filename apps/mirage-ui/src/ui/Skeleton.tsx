import { cx } from './cx';

export function Skeleton({ className }: { className?: string }) {
  return <span className={cx('skeleton', className)} aria-hidden="true" />;
}

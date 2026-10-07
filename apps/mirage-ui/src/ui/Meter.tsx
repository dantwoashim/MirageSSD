import { formatBytes } from '../format';
import { cx } from './cx';

export type MeterTone = 'accent' | 'ok' | 'warn' | 'danger' | 'neutral' | 'track';

export type MeterSegment = {
  value: number;
  tone: MeterTone;
  label: string;
  /** false keeps the segment out of the bar so it only appears in the legend. */
  bar?: boolean;
};

const toneClasses: Record<MeterTone, string> = {
  accent: 'bg-accent',
  ok: 'bg-ok',
  warn: 'bg-warn',
  danger: 'bg-danger',
  neutral: 'bg-line-strong',
  track: 'border border-line-strong bg-surface-2',
};

/**
 * Segmented bar. Without `total`, segments fill the bar proportionally; with
 * it, each segment is value/total and the rest stays as empty track.
 */
export function Meter({ segments, total, label, legend = true }: { segments: MeterSegment[]; total?: number; label: string; legend?: boolean }) {
  const barSegments = segments.filter((segment) => segment.bar !== false);
  const barTotal = Math.max(0, total ?? barSegments.reduce((sum, segment) => sum + Math.max(0, segment.value), 0));
  return (
    <div>
      <div className="flex h-2 overflow-hidden rounded-full bg-surface-2" role="img" aria-label={label}>
        {barTotal > 0 &&
          barSegments
            .filter((segment) => segment.value > 0)
            .map((segment) => (
              <span key={segment.label} className={toneClasses[segment.tone]} style={{ width: `${Math.min(100, (segment.value / barTotal) * 100)}%` }} />
            ))}
      </div>
      {legend && (
        <ul className="mt-2.5 flex flex-wrap gap-x-4 gap-y-1">
          {segments.map((segment) => (
            <li key={segment.label} className="flex items-center gap-1.5 text-xs text-fg-muted">
              <span className={cx('size-2 rounded-[3px]', toneClasses[segment.tone])} aria-hidden="true" />
              {segment.label}
              <span className="tabular text-fg">{formatBytes(segment.value)}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

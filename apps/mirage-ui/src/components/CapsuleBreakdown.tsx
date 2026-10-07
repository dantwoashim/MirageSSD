import type { Readiness } from '../models';
import { formatBytes } from '../format';
import { cx } from '../ui';
export { formatBytes } from '../format';

const fields: Array<[keyof Readiness, string, string]> = [
  ['hardSetBytes', 'Hard set', 'Pages required in every supported session'],
  ['envelopeBytes', 'Session envelope', 'Observed recurring working set'],
  ['scanMapBytes', 'Scan and map set', 'File-wide scans and mapped ranges'],
  ['frontierBytes', 'Safety frontier', 'Held-out protection around likely paths'],
  ['updateReserveBytes', 'Update reserve', 'Space kept aside for safe version changes'],
  ['missingBytes', 'Missing locally', 'Bytes that still block offline access'],
];

export function CapsuleBreakdown({ value }: { value: Readiness }) {
  return (
    <dl className="divide-y divide-line border-y border-line">
      {fields.map(([key, name, description]) => (
        <div key={key} className="grid grid-cols-[1fr_auto] gap-5 py-3">
          <div>
            <dt className="text-[13px] font-medium text-fg">{name}</dt>
            <p className="mt-0.5 text-xs leading-relaxed text-fg-subtle">{description}</p>
          </div>
          <dd className={cx('number self-center text-sm', key === 'missingBytes' && value.missingBytes > 0 ? 'text-warn' : 'text-fg-muted')}>
            {formatBytes(Number(value[key]))}
          </dd>
        </div>
      ))}
    </dl>
  );
}

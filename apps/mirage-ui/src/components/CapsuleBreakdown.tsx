import type { Readiness } from '../models';

const fields: Array<[keyof Readiness, string, string]> = [
  ['hardSetBytes', 'Hard set', 'Pages required in every supported session'],
  ['envelopeBytes', 'Session envelope', 'Observed recurring working set'],
  ['scanMapBytes', 'Scan and map set', 'File-wide scans and mapped ranges'],
  ['frontierBytes', 'Safety frontier', 'Held-out protection around likely paths'],
  ['updateReserveBytes', 'Update reserve', 'Space retained for safe generation changes'],
  ['missingBytes', 'Missing locally', 'Bytes that still block sealed admission'],
];

export function CapsuleBreakdown({ value }: { value: Readiness }) {
  return (
    <dl className="divide-y divide-white/6 border-y border-white/8">
      {fields.map(([key, name, description]) => (
        <div key={key} className="grid grid-cols-[1fr_auto] gap-5 py-3.5">
          <div>
            <dt className="text-sm font-medium text-zinc-100">{name}</dt>
            <p className="mt-0.5 text-xs leading-relaxed text-zinc-500">{description}</p>
          </div>
          <dd className={`number self-center text-sm ${key === 'missingBytes' && value.missingBytes > 0 ? 'text-amber-200' : 'text-zinc-300'}`}>
            {formatBytes(Number(value[key]))}
          </dd>
        </div>
      ))}
    </dl>
  );
}

export function formatBytes(bytes: number | null): string {
  if (bytes === null || !Number.isFinite(bytes)) return 'Not measured';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const unit = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  const value = bytes / 1024 ** unit;
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

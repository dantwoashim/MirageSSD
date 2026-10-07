import type { ThemePreference } from '../theme';
import { setThemePreference, useTheme } from '../theme';
import { cx } from '../ui';

const options: { id: ThemePreference; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
];

export function ThemeToggle({ labelled = false }: { labelled?: boolean }) {
  const { preference } = useTheme();
  return (
    <div role="radiogroup" aria-label="Theme" className="inline-flex items-center gap-1 rounded-lg border border-line bg-surface-2 p-1">
      {labelled && <span className="px-1.5 text-xs text-fg-muted">Theme</span>}
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          role="radio"
          aria-checked={preference === option.id}
          className={cx(
            'rounded-md px-2.5 py-1 text-xs transition-colors duration-150 ease-standard',
            preference === option.id ? 'bg-accent font-medium text-on-accent' : 'text-fg-muted hover:text-fg',
          )}
          onClick={() => setThemePreference(option.id)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

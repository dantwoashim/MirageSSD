import type { ThemePreference } from '../theme';
import { setThemePreference, useTheme } from '../theme';

const options: { id: ThemePreference; label: string }[] = [
  { id: 'system', label: 'System' },
  { id: 'light', label: 'Light' },
  { id: 'dark', label: 'Dark' },
];

export function ThemeToggle({ labelled = false }: { labelled?: boolean }) {
  const { preference } = useTheme();
  return (
    <div className="theme-toggle" role="radiogroup" aria-label="Theme">
      {labelled && <span className="theme-toggle-label">Theme</span>}
      {options.map((option) => (
        <button
          key={option.id}
          role="radio"
          aria-checked={preference === option.id}
          className={preference === option.id ? 'theme-option is-active' : 'theme-option'}
          onClick={() => setThemePreference(option.id)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

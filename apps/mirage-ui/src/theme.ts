import { useSyncExternalStore } from 'react';

export type ThemePreference = 'system' | 'light' | 'dark';
export type ResolvedTheme = 'light' | 'dark';

const STORAGE_KEY = 'mirage.theme';
const media = () => window.matchMedia('(prefers-color-scheme: light)');
const inBrowser = typeof window !== 'undefined';

function queryTheme(): ThemePreference | undefined {
  if (!inBrowser) return undefined;
  const value = new URLSearchParams(window.location.search).get('theme');
  return value === 'light' || value === 'dark' || value === 'system' ? value : undefined;
}

function storedPreference(): ThemePreference {
  if (!inBrowser) return 'system';
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    if (value === 'light' || value === 'dark' || value === 'system') return value;
  } catch {
    /* Private-mode storage denial keeps the default. */
  }
  return 'system';
}

const listeners = new Set<() => void>();
let preference: ThemePreference = queryTheme() ?? storedPreference();
let resolved: ResolvedTheme =
  preference === 'system' ? (inBrowser && media().matches ? 'light' : 'dark') : preference;

function apply() {
  resolved = preference === 'system' ? (media().matches ? 'light' : 'dark') : preference;
  if (inBrowser) {
    document.documentElement.dataset.theme = resolved;
    document.documentElement.style.colorScheme = resolved;
  }
}

export function initTheme() {
  if (!inBrowser) return;
  apply();
  media().addEventListener('change', () => {
    if (preference === 'system') {
      apply();
      listeners.forEach((listener) => listener());
    }
  });
}

export function setThemePreference(next: ThemePreference) {
  preference = next;
  try {
    if (inBrowser) window.localStorage.setItem(STORAGE_KEY, next);
  } catch {
    /* Session-only preference when storage is denied. */
  }
  apply();
  listeners.forEach((listener) => listener());
}

export function useTheme(): { preference: ThemePreference; resolved: ResolvedTheme } {
  const snapshot = useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => `${preference}|${resolved}`,
    () => 'system|dark',
  );
  const [p, r] = snapshot.split('|');
  return { preference: p as ThemePreference, resolved: r as ResolvedTheme };
}

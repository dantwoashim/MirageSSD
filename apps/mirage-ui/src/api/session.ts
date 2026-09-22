const KEY = 'miragessd.bridge-session';
const valid = (value: string | null): value is string => value !== null && /^[0-9a-f]{64}$/.test(value);

// A tab-scoped capability survives a reload without entering server requests,
// localStorage, analytics, or URLs copied after the initial launch.
export function bridgeSession(hash: string, storage?: Pick<Storage, 'getItem' | 'setItem'>) {
  const supplied = hash.replace(/^#/, '');
  if (supplied) {
    if (!valid(supplied)) return { token: '', removeHash: false };
    try {
      storage?.setItem(KEY, supplied);
      return { token: supplied, removeHash: Boolean(storage) };
    } catch {
      return { token: supplied, removeHash: false };
    }
  }
  try {
    const cached = storage?.getItem(KEY) ?? null;
    return { token: valid(cached) ? cached : '', removeHash: false };
  } catch {
    return { token: '', removeHash: false };
  }
}

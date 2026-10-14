import { useEffect, useState } from 'react';
import type { ServiceClient } from '../api/client';
import { Button, Card, Field, Input, Notice, Select, Spinner } from '../ui';

const GiB = 1024 ** 3;
export const validGameBudgets = (allowance: string, cache: string) => {
  const total = Number(allowance); const owned = Number(cache);
  return Number.isSafeInteger(total) && Number.isSafeInteger(owned) && total >= 1 && total <= 1024 && owned >= 1 && owned <= total;
};
const bytes = (value: unknown) => typeof value === 'number' ? `${(value / GiB).toFixed(2)} GiB` : 'Measuring…';
const phases: Record<string, string> = {
  acquiring_metadata: 'Checking official publisher files', downloading_native_files: 'Downloading and verifying native files',
  preparing: 'Preparing learned launch data', waiting_for_game: 'Waiting for the game to start', playing: 'Playing',
  cleanup_pending: 'Cleanup pending', ready: 'Ready', installation_failed: 'Installation needs attention',
  maintenance_failed: 'Update needs attention', launch_failed: 'Launch needs attention',
};

export function Games({ client }: { client: ServiceClient }) {
  const [state, setState] = useState<Record<string, unknown>>();
  const [error, setError] = useState<string>();
  const [submitting, setSubmitting] = useState(false);
  const [root, setRoot] = useState('');
  const [seed, setSeed] = useState('');
  const [language, setLanguage] = useState('en-us');
  const [retention, setRetention] = useState('balanced');
  const [allowance, setAllowance] = useState('40');
  const [cache, setCache] = useState('4');
  const [connect, setConnect] = useState(false);
  const refresh = async () => {
    try { setState(await client.gameStatus()); setError(undefined); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
  };
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try { const value = await client.gameStatus(); if (!cancelled) { setState(value); setError(undefined); } }
      catch (failure) { if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure)); }
    };
    void poll(); const interval = window.setInterval(() => void poll(), 2500);
    return () => { cancelled = true; window.clearInterval(interval); };
  }, [client]);
  const action = async (operation: string, options?: unknown) => {
    setSubmitting(true); setError(undefined);
    try {
      const spec = operation === 'install' ? {
        root: root.trim(), seed_root: connect ? seed.trim() : null, language, retention,
        total_budget_bytes: Number(allowance) * GiB, cache_budget_bytes: Number(cache) * GiB,
        startup_budget_bytes: Math.min(Number(cache), 0.5) * GiB, disk_free_floor_bytes: 2 * GiB, launcher: null,
      } : options;
      await client.gameAction(operation, spec); await refresh();
    } catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setSubmitting(false); }
  };
  const busy = submitting || state?.busy === true;
  const registered = state?.registered === true;
  const failed = typeof state?.error === 'string' ? state.error : undefined;
  const phase = typeof state?.phase === 'string' ? phases[state.phase] ?? state.phase.replaceAll('_', ' ') : 'Not installed';
  const valid = Boolean(root.trim()) && (!connect || Boolean(seed.trim())) && validGameBudgets(allowance, cache);
  return <section className="grid gap-4" aria-labelledby="genshin-title">
    <Card padding="lg">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div><p className="field-label">Experimental game integration</p><h2 id="genshin-title" className="section-title mt-1">Genshin Impact</h2>
          <p className="mt-2 max-w-2xl text-[13px] leading-relaxed text-fg-muted">Native game files stay on this PC. Immutable asset blocks, audio and video come from the official publisher CDN as needed. Unseen assets can require a download during play.</p></div>
        <span role="status" className="flex items-center gap-2 text-sm text-fg-muted">{busy && <Spinner size={15} />}{phase}</span>
      </div>
      <Notice tone="warning" title="Compatibility and gameplay measurements are still pending" className="mt-4">
        This preview downloads and verifies real publisher files. It does not guarantee smoother gameplay, official launcher repair compatibility, or a particular storage saving. Original installations are retained when connected.
      </Notice>
    </Card>
    {(error || failed) && <Notice tone="danger" title="Game operation needs attention">{error ?? failed}</Notice>}
    {registered ? <>
      <Card padding="lg">
        <dl className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {[
            ['Native installation', state.native_bytes], ['Owned cache and metadata', state.owned_cache_and_metadata_bytes],
            ['Temporary and rollback space', state.temporary_and_rollback_bytes], ['Imported original data', state.imported_original_bytes],
            ['Total physical footprint', state.physical_total_bytes], ['Available drive space', state.available_bytes],
          ].map(([label, value]) => <div key={String(label)}><dt className="field-label">{String(label)}</dt><dd className="number mt-1 text-lg text-fg">{bytes(value)}</dd></div>)}
        </dl>
        <p className="mt-5 text-xs text-fg-muted">Virtual asset length: {bytes(state.virtual_logical_bytes)}. This is advertised file size and is not added to the physical footprint.</p>
        <p className="mt-2 text-xs text-fg-muted">{state.retention === 'disposable' ? 'Disposable: clear all owned asset chunks after the game exits. Native files, user data and bounded access history stay local.' : 'Balanced: retain the prepared launch set and clear other owned asset chunks after the game exits.'}</p>
        <Field className="mt-4 max-w-sm" label="Retention for the next session" htmlFor="game-next-policy"><Select id="game-next-policy" value={String(state.retention ?? 'balanced')} disabled={busy} onChange={e => void action('policy', { retention: e.target.value })}><option value="balanced">Balanced</option><option value="disposable">Disposable</option></Select></Field>
        <p className="mt-2 break-all text-xs text-fg-subtle">Native folder: {String(state.native_root ?? 'Preparing…')}</p>
      </Card>
      <div className="flex flex-wrap gap-2">
        <Button disabled={busy || !state.native_root} onClick={() => void action('play')}>Prepare and play</Button>
        <Button variant="secondary" disabled={busy} onClick={() => void action('update')}>Check and stage update</Button>
        <Button variant="secondary" disabled={busy} onClick={() => void action('repair')}>Repair native files</Button>
        <Button variant="secondary" disabled={busy || state.rollback_available !== true} onClick={() => void action('rollback')}>Use previous generation</Button>
        <Button variant="ghost" disabled={busy} onClick={() => void action('cleanup')}>Retry cleanup</Button>
        <Button variant="ghost" disabled={busy || state.retirement_available !== true} onClick={() => void action('retire')}>Remove previous native files</Button>
      </div>
      <p className="text-xs text-fg-muted">The session helper keeps running if you close this window. Cleanup waits for the actual game process to exit. Update and repair stage a new native generation; the previous generation uses disk space until you remove its recorded native files. Removing them disables rollback and keeps user files and originals.</p>
    </> : <Card padding="lg">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Setup" htmlFor="game-setup"><Select id="game-setup" value={connect ? 'connect' : 'fresh'} disabled={busy} onChange={e => setConnect(e.target.value === 'connect')}><option value="fresh">Fresh managed installation</option><option value="connect">Connect existing installation</option></Select></Field>
        <Field label="Voice language" htmlFor="game-language"><Select id="game-language" value={language} disabled={busy} onChange={e => setLanguage(e.target.value)}><option value="en-us">English</option><option value="ja-jp">Japanese</option><option value="ko-kr">Korean</option><option value="zh-cn">Chinese</option></Select></Field>
        <Field label="New managed folder" htmlFor="game-root" hint="Enter a new absolute folder on your chosen drive."><Input id="game-root" value={root} disabled={busy} placeholder="D:\Games\MirageGenshin" onChange={e => setRoot(e.target.value)} /></Field>
        {connect && <Field label="Original game folder" htmlFor="game-seed" hint="Read-only reuse. Original files count toward the total footprint and stay untouched."><Input id="game-seed" value={seed} disabled={busy} placeholder="D:\Games\Genshin Impact game" onChange={e => setSeed(e.target.value)} /></Field>}
        <Field label="Total game allowance (GiB)" htmlFor="game-total"><Input id="game-total" type="number" min="1" max="1024" value={allowance} disabled={busy} onChange={e => setAllowance(e.target.value)} /></Field>
        <Field label="Owned asset cache (GiB)" htmlFor="game-cache"><Input id="game-cache" type="number" min="1" max="1024" value={cache} disabled={busy} onChange={e => setCache(e.target.value)} /></Field>
        <Field label="After the game exits" htmlFor="game-retention"><Select id="game-retention" value={retention} disabled={busy} onChange={e => setRetention(e.target.value)}><option value="balanced">Balanced — retain learned launch data</option><option value="disposable">Disposable — clear all owned chunks</option></Select></Field>
      </div>
      <p className="mt-4 text-xs leading-relaxed text-fg-muted">Installation acquires the current publisher generation and selected voice pack, then downloads and verifies local native files. It does not download every streamed asset first. Preparation retains at most 0.5 GiB of learned data within your cache allowance. Keep at least 2 GiB free for the volume; update staging may require a larger allowance.</p>
      <Button className="mt-4" disabled={busy || !valid || !state} onClick={() => void action('install')}>{connect ? 'Connect and prepare managed installation' : 'Download and install'}</Button>
    </Card>}
  </section>;
}

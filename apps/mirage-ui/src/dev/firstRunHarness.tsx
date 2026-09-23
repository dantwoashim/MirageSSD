// Dev-only harness (not part of the production bundle): renders the real App
// against a simulated fresh PC — no drives, signed out — and scripts the
// Google sign-in completing, so the first-run flow can be checked headlessly.
import { createRoot } from 'react-dom/client';
import { App } from '../App';
import { ServiceClient, type LocalBridge } from '../api/client';
import type { Request, Response } from '../models';
import '../styles.css';

const log = (line: string) => {
  const element = document.getElementById('harness-log');
  if (element) element.textContent += `${line}\n`;
};

let signedIn = false;
let signInRequested = 0;
let created = false;
let createPolls = 0;

const repositories = () => created
  ? [{ repository_id: 'ab'.repeat(16), display_name: 'MirageSSD', state: 'ready_mounted', active_generation: 0, active_commit: 'cd'.repeat(32) }]
  : [];

const bridge: LocalBridge = {
  async invoke(request: Request): Promise<Response> {
    const command = (request as unknown as { command: { command: string } }).command.command;
    let value: unknown = {};
    if (command === 'status') value = { service: 'running', configured: created, repositories: repositories() };
    if (command === 'repository_detail') value = { ...repositories()[0], origin: 'drive', volume_mode: 'managed', mount_path: 'M:' };
    return { protocol_version: 3, request_id: request.request_id, body: { kind: 'json', value } } as Response;
  },
  async apiGet(path: string) {
    if (path === '/api/drive/status') {
      // Sign-in completes a few polls after the user starts it.
      if (signInRequested > 0 && ++signInRequested > 3) signedIn = true;
      return { authenticated: signedIn, account_id: signedIn ? 'friend@example.com' : null, issued_unix_seconds: signedIn ? Math.floor(Date.now() / 1000) : null, login: signInRequested > 0 && !signedIn ? 'in_flight' : 'idle' };
    }
    if (path === '/api/disks') {
      return { disks: [{ volume_root: 'C:\\', total_bytes: 256 * 2 ** 30, free_bytes: 90 * 2 ** 30 }], state_volume: { volume_root: 'C:\\', total_bytes: 256 * 2 ** 30, free_bytes: 90 * 2 ** 30 }, default_letter: 'M', free_letters: ['M', 'N', 'O'], default_budget_bytes: 22 * 2 ** 30, default_cache_disk: 'C:\\' };
    }
    if (path === '/api/volume/create-status') {
      createPolls += 1;
      if (createPolls >= 3) { created = true; return { in_flight: false, done: true, drive_letter: 'M', name: 'MirageSSD', repository_id: 'ab'.repeat(16) }; }
      return { in_flight: true, step: 'Publishing to Google Drive' };
    }
    if (path === '/api/update-check') return {};
    return {};
  },
  async apiPost(path: string, body?: unknown) {
    log(`POST ${path} ${body ? JSON.stringify(body) : ''}`);
    if (path === '/api/drive/login') signInRequested = 1;
    if (path === '/api/volume/create') return { started: true };
    return {};
  },
};

createRoot(document.getElementById('root')!).render(<App client={new ServiceClient(bridge)} />);

// Script the user: a fresh PC shows setup; the user clicks "Sign in with Google".
const clickSignIn = () => {
  const button = [...document.querySelectorAll('button')].find((b) => b.textContent?.includes('Sign in with Google'));
  if (button) { log('CLICK Sign in with Google'); button.click(); } else window.setTimeout(clickSignIn, 200);
};
window.setTimeout(clickSignIn, 500);

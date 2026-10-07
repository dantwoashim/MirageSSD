import { createRoot } from 'react-dom/client';
import { App } from '../App';
import { ServiceClient } from '../api/client';
import { ToastProvider } from '../ui';
import { initTheme } from '../theme';
import { mockBridge } from './mockBridge';
import '../styles.css';

// First-run automation harness: dev page that clicks through the wizard
// against the 'fresh' mock and logs each step to the DOM so a Playwright
// spec can assert on it. Everything here is dev-only.
initTheme();

const log = (line: string) => {
  const el = document.createElement('div');
  el.setAttribute('data-log', 'true');
  el.textContent = line;
  document.body.appendChild(el);
};

const base = mockBridge('fresh');
const client = new ServiceClient({
  ...base,
  apiPost: (path, body) => {
    log(`POST ${path} ${JSON.stringify(body ?? {})}`);
    return base.apiPost!(path, body);
  },
});
log('mounted');

createRoot(document.getElementById('root')!).render(<ToastProvider><App client={client} /></ToastProvider>);

const click = async (selector: string) => {
  for (let attempt = 0; attempt < 240; attempt++) {
    const element = document.querySelector<HTMLElement>(selector);
    if (element && !(element as HTMLButtonElement).disabled) {
      element.click();
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  log(`timeout waiting for ${selector}`);
};

const waitFor = async (predicate: () => boolean) => {
  for (let attempt = 0; attempt < 240; attempt++) {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return false;
};

void (async () => {
  log('page ready, waiting for sign-in step');
  await click('button:not([disabled])'); // "Sign in with Google"
  log('clicked sign in');
  await waitFor(() => document.body.textContent?.includes('Creating the drive') || document.body.textContent?.includes('Your drive is ready') || false);
  log('create step reached (auto-create on fresh install)');
  const ready = await waitFor(() => document.body.textContent?.includes('Your drive is ready') ?? false);
  log(ready ? 'drive ready' : 'drive did not reach ready state');
})();

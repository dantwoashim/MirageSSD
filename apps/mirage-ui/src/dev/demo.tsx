import { createRoot } from 'react-dom/client';
import { App } from '../App';
import { ServiceClient } from '../api/client';
import { ToastProvider } from '../ui';
import { initTheme } from '../theme';
import { mockBridge, type DemoScenario } from './mockBridge';
import '../styles.css';

initTheme();

const scenario = (new URLSearchParams(window.location.search).get('scenario') ?? 'drive') as DemoScenario;
const client = new ServiceClient(mockBridge(scenario));

createRoot(document.getElementById('root')!).render(
  <ToastProvider>
    <App client={client} />
  </ToastProvider>,
);

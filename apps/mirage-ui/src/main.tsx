import { StrictMode } from 'react';
import { MotionConfig } from 'framer-motion';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { FetchBridge, ServiceClient } from './api/client';
import { bridgeSession } from './api/session';
import { initTheme } from './theme';
import './styles.css';

const root = document.getElementById('root');
if (!root) throw new Error('MirageSSD UI root is missing');

let storage: Storage | undefined;
try { storage = window.sessionStorage; } catch { /* The launch fragment remains usable. */ }
initTheme();
const session = bridgeSession(window.location.hash, storage);
const bridge = new FetchBridge(session.token);
if (session.removeHash) window.history.replaceState(null, '', window.location.pathname + window.location.search);
createRoot(root).render(
  <StrictMode>
    <MotionConfig reducedMotion="user"><App client={new ServiceClient(bridge)} /></MotionConfig>
  </StrictMode>,
);

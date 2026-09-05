import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { FetchBridge, ServiceClient } from './api/client';
import './styles.css';

const root = document.getElementById('root');
if (!root) throw new Error('MirageSSD UI root is missing');

const bridge = new FetchBridge(window.location.hash.slice(1));
window.history.replaceState(null, '', window.location.pathname);
createRoot(root).render(
  <StrictMode>
    <App client={new ServiceClient(bridge)} />
  </StrictMode>,
);

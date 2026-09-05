import { createElement as h } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { mkdir, writeFile, copyFile } from 'node:fs/promises';
import { Layout, Home } from './components.mjs';

const origin = 'https://miragessd.prabinghimire1.com.np';
const repo = 'https://github.com/dantwoashim/MirageSSD';
const updated = '5 September 2026';
const link = (href, text, className = 'underline hover:text-leaf') => h('a', { href, className }, text);
const p = (...children) => h('p', { className: 'max-w-[65ch] text-muted leading-relaxed' }, ...children);
const section = (title, ...children) => h('section', { className: 'space-y-4 border-t border-ink/15 py-8' }, h('h2', { className: 'text-xl font-semibold tracking-tight' }, title), ...children);


function Legal({ title, intro, children }) {
  return h('article', { className: 'mx-auto max-w-3xl px-6 py-14 sm:py-20' },
    h('p', { className: 'mb-4 text-xs uppercase tracking-[.18em] text-leaf' }, `Last updated · ${updated}`),
    h('h1', { className: 'mb-6 text-4xl font-semibold tracking-tight sm:text-5xl' }, title),
    h('div', { className: 'mb-10' }, p(intro)), children);
}

const privacy = h(Legal, { title: 'Privacy policy', intro: 'This policy describes the MirageSSD Windows preview and this public website, maintained by the MirageSSD project at github.com/dantwoashim/MirageSSD.' },
  section('Google access and its purpose', p('MirageSSD requests https://www.googleapis.com/auth/drive.file. It accesses file contents and metadata for files created with, opened with, or otherwise authorized for the app. It uses that access to list, read, upload, update, and delete files as needed for the mounted drive and operations you initiate. It also reads account identifiers and storage quota information to identify the connected account and report capacity. It does not request your Google password.')),
  section('What stays on your device', p('OAuth access and refresh tokens are stored locally so the app can connect and refresh authorization. The application’s token store uses Windows protection mechanisms; authorized provider processes receive credentials while running. Local cache data includes staged uploads and downloaded file contents. Local metadata includes file attributes and operational state. Diagnostic logs can include paths, filenames, transfer details, and error messages.'), p('Ordinary writable-drive files and cached contents are not client-side encrypted by MirageSSD. Protect your Windows account and local disk. Do not share credentials or unredacted logs.')),
  section('Where data goes and who can access it', p('File contents and Google authorization requests are transmitted to Google’s services over HTTPS. The writable mount uses rclone and WinFsp locally. This website is not a file-transfer proxy and does not receive your Drive tokens or mounted file contents. MirageSSD maintainers do not receive your files through normal mount operation. Information you voluntarily submit in a support request is received through that support channel; public GitHub issues are visible to others.'), p('Google processes information under its own policies. Hosting and network providers, including Vercel and Cloudflare where used, may process website request information such as IP address, requested URL, timestamp, and browser details to deliver and secure this website.')),
  section('No advertising use or AI training', p('MirageSSD does not sell Google user data, use it for advertising, or use it to train generalized AI or machine-learning models. MirageSSD’s use and transfer of information received from Google APIs adheres to the Google API Services User Data Policy, including the Limited Use requirements.'), link('https://developers.google.com/terms/api-services-user-data-policy', 'Google API Services User Data Policy')),
  section('Retention, disconnection, and deletion', p('Cloud files remain in your Google account until deleted by you or by an operation you authorize. Cache cleanup depends on local space and upload state; pending writes may remain indefinitely until resolved. Uninstalling the preview intentionally retains credentials, cache, metadata, and cloud files to avoid unintended data loss.'), p('To stop access, stop the mount and remove MirageSSD’s authorization in your Google Account connections. Revocation prevents future authorized access but does not delete files already stored locally or in Drive. Delete cloud files through Google Drive when you no longer need them. Remove retained local data only after uploads and a restore are verified, and after stopping MirageSSD. Contact the maintainer for help identifying these files.'), link('https://myaccount.google.com/connections', 'Manage Google Account connections')),
  section('Website cookies and support', p('This website has no application login, forms, advertising trackers, or analytics scripts. Infrastructure providers may retain operational logs according to their policies. If you contact support, avoid posting personal files, tokens, or private paths. Request a private channel before sharing sensitive information.'), link(`${repo}/issues`, 'Contact the MirageSSD maintainer through GitHub')),
  section('Policy changes', p('This page will be updated when the described practices change. The date above identifies the current revision. Review the policy before installing a new preview build.')));

const terms = h(Legal, { title: 'Terms of use', intro: 'These terms describe use of the MirageSSD website and preview software. The software’s Apache-2.0 license and applicable third-party licenses govern your rights to use, modify, and distribute the code.' },
  section('An engineering preview', p('MirageSSD is provided for evaluation. It may contain defects, interrupt transfers, or lose data. No uninterrupted availability, transfer speed, compatibility, backup durability, or fitness for a particular purpose is promised. The name does not mean that network storage performs like a physical SSD.')),
  section('Your account and your files', p('Use only accounts and data you are authorized to access. You remain responsible for account security, Google storage charges and quota, local free space, lawful content, and compliance with Google’s terms. MirageSSD is independent and is not endorsed by Google, Microsoft, Vercel, or Cloudflare.')),
  section('Safe operation', p('Maintain independent backups. Verify uploads and restores before deleting originals. Never delete cached data while uploads are pending. Use native storage for games, virtual machines, databases, and other workloads that need predictable local-disk behavior. Stop using the preview if errors threaten your data.')),
  section('License and warranty', p('MirageSSD source is distributed under Apache License 2.0, including its warranty disclaimer and limitation of liability. Dependencies remain subject to their own licenses. Nothing on this page removes rights or protections that applicable law does not allow to be excluded.'), link(`${repo}/blob/main/LICENSE`, 'Read the software license')),
  section('Third-party services and ending use', p('Google Drive and hosting services operate under their own terms and may change or restrict access. You may stop using MirageSSD at any time. Uninstalling does not automatically delete cloud files or retained local data. Follow the privacy policy’s disconnection and deletion guidance.'), link('/privacy', 'Privacy and deletion guidance')),
  section('Contact and updates', p('Report problems through the project’s support channel. Do not submit passwords, OAuth tokens, or confidential files in a public issue. Updated terms will be published here with a revised date.'), link(`${repo}/issues`, 'Project support')));

const pages = [
  ['index.html', '/', 'MirageSSD — Google Drive in Windows Explorer', 'Mount app-authorized Google Drive storage as a writable Windows drive, with a local write-back cache. Open-source engineering preview.', h(Home)],
  ['privacy.html', '/privacy', 'Privacy policy — MirageSSD', 'How MirageSSD accesses Google Drive data, stores local cache and tokens, and handles disconnection and deletion.', privacy],
  ['terms.html', '/terms', 'Terms of use — MirageSSD', 'Terms, preview limitations, data safety, and open-source licensing for MirageSSD.', terms],
  ['404.html', '/404', 'Page not found — MirageSSD', 'This page could not be found.', h(Legal, { title: 'Page not found', intro: 'That address does not point to a page on this site.' }, link('/', 'Return to MirageSSD'))]
];
await mkdir('dist', { recursive: true });
await copyFile('node_modules/@fontsource-variable/geist/files/geist-latin-wght-normal.woff2', 'dist/geist-latin.woff2');
await copyFile('node_modules/@fontsource-variable/geist/LICENSE', 'dist/geist-license.txt');
await copyFile('node_modules/@phosphor-icons/react/LICENSE', 'dist/phosphor-license.txt');
await writeFile('dist/favicon.svg', '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 40"><rect width="40" height="40" rx="10" fill="#202923"/><path d="m10 13 10-5 10 5-10 5zm0 7 10 5 10-5M10 27l10 5 10-5" fill="none" stroke="#e3ebe5" stroke-width="2" stroke-linejoin="round"/></svg>');
for (const [filename, path, title, description, content] of pages) {
  await writeFile(`dist/${filename}`, '<!doctype html>' + renderToStaticMarkup(h(Layout, { title, description, path }, content)));
}
await writeFile('dist/robots.txt', `User-agent: *\nAllow: /\nSitemap: ${origin}/sitemap.xml\n`);
await writeFile('dist/sitemap.xml', `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${pages.slice(0, 3).map(([, path]) => `<url><loc>${origin}${path}</loc></url>`).join('')}</urlset>`);
console.log('Built homepage, privacy, terms, and 404 as static HTML. No client-side JavaScript.');

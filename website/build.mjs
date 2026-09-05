import { createElement as h } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { mkdir, writeFile } from 'node:fs/promises';

const origin = 'https://miragessd.prabinghimire1.com.np';
const repo = 'https://github.com/dantwoashim/MirageSSD';
const updated = '5 September 2026';
const link = (href, text, className = 'underline hover:text-leaf') => h('a', { href, className }, text);
const p = (...children) => h('p', { className: 'max-w-[65ch] text-muted leading-relaxed' }, ...children);
const section = (title, ...children) => h('section', { className: 'space-y-4 border-t border-ink/15 py-8' }, h('h2', { className: 'text-xl font-semibold tracking-tight' }, title), ...children);
const button = (href, label, secondary = false) => link(href, label, `inline-flex items-center justify-center rounded-full px-6 py-3 font-medium transition-transform active:scale-[.98] ${secondary ? 'border border-ink/25 hover:border-leaf' : 'bg-leaf text-white hover:bg-ink'}`);

function Layout({ title, description, path, children }) {
  return h('html', { lang: 'en' },
    h('head', null, h('meta', { charSet: 'utf-8' }), h('meta', { name: 'viewport', content: 'width=device-width, initial-scale=1' }),
      h('title', null, title), h('meta', { name: 'description', content: description }),
      h('link', { rel: 'canonical', href: `${origin}${path}` }), h('link', { rel: 'stylesheet', href: '/site.css' }),
      h('meta', { property: 'og:title', content: title }), h('meta', { property: 'og:description', content: description }),
      h('meta', { property: 'og:type', content: 'website' }), h('meta', { property: 'og:url', content: `${origin}${path}` })),
    h('body', null,
      link('#main', 'Skip to content', 'sr-only focus:not-sr-only focus:block focus:p-4'),
      h('header', { className: 'mx-auto flex max-w-6xl flex-wrap items-center justify-between gap-5 border-b border-ink/15 px-6 py-6 sm:px-10' },
        link('/', 'MirageSSD', 'text-xl font-semibold tracking-tight'),
        h('nav', { 'aria-label': 'Main navigation', className: 'flex flex-wrap gap-5 text-sm' }, link('/#how-it-works', 'How it works'), link('/privacy', 'Privacy'), link(repo, 'Source code'))),
      h('main', { id: 'main', className: 'mx-auto max-w-6xl px-6 sm:px-10' }, children),
      h('footer', { className: 'mx-auto mt-14 flex max-w-6xl flex-wrap justify-between gap-6 border-t border-ink/15 px-6 py-8 text-sm text-muted sm:px-10' },
        h('p', null, 'MirageSSD · An independent open-source project'),
        h('nav', { 'aria-label': 'Legal and support', className: 'flex flex-wrap gap-5' }, link('/privacy', 'Privacy policy'), link('/terms', 'Terms of use'), link(`${repo}/issues`, 'Support')))));
}

function Home() {
  return h('div', null,
    h('section', { className: 'grid gap-12 py-16 md:grid-cols-[1.35fr_1fr] md:items-center md:py-24' },
      h('div', { className: 'space-y-7' }, h('p', { className: 'text-xs font-semibold uppercase tracking-[.2em] text-leaf' }, 'Windows 11 · Engineering preview'),
        h('h1', { className: 'max-w-xl text-4xl font-semibold leading-[1.08] tracking-tight sm:text-6xl' }, 'Your cloud storage.', h('br'), 'A familiar drive.'),
        p('MirageSSD mounts an application-owned folder in your Google Drive as a writable Windows drive. Open files, save downloads, and move folders from Explorer—with a local cache for recently used data.'),
        h('div', { className: 'flex flex-wrap gap-3' }, button(`${repo}#get-started`, 'Get started'), button('/#how-it-works', 'Understand the cache', true)),
        h('p', { className: 'text-sm text-muted' }, 'Open source. Your Google account. Your existing storage quota.')),
      h('figure', { className: 'rounded-3xl border border-ink/15 bg-white p-7 shadow-sm sm:p-9' },
        h('figcaption', { className: 'mb-8 text-xs uppercase tracking-[.18em] text-muted' }, 'The storage path · illustration'),
        ...[['01', 'Windows Explorer', 'Read and write through a drive letter.'], ['02', 'Your local cache', 'Stage writes. Reuse downloaded content.'], ['03', 'Your Google Drive', 'Upload in the background. Fetch when needed.']].map(([n, title, text]) =>
          h('div', { key: n, className: 'flex gap-5 border-t border-ink/10 py-6' }, h('span', { className: 'font-mono text-sm text-leaf' }, n), h('div', null, h('p', { className: 'mb-1 font-semibold' }, title), h('p', { className: 'text-sm leading-relaxed text-muted' }, text)))))),
    h('section', { id: 'how-it-works', className: 'grid gap-8 border-t border-ink/15 py-12 md:grid-cols-[1fr_1.6fr]' },
      h('div', null, h('p', { className: 'mb-4 text-xs uppercase tracking-[.18em] text-leaf' }, 'Built around your disk'), h('h2', { className: 'text-3xl font-semibold tracking-tight' }, 'Fast where cached.', h('br'), 'Honest everywhere.')),
      h('div', { className: 'space-y-7' }, p('Cached reads use local storage. New writes are staged locally before they reach Google Drive. Uncached reads and completed uploads still depend on your connection and Google’s service limits.'),
        p('The cache uses real disk space. Pending uploads cannot safely be evicted. A completed copy into the drive is not confirmation that the cloud upload has finished.'),
        link(`${repo}/blob/main/docs/architecture.md`, 'Read the architecture'))),
    h('section', { className: 'grid gap-8 border-t border-ink/15 py-12 md:grid-cols-[1fr_1.6fr]' },
      h('h2', { className: 'text-3xl font-semibold tracking-tight' }, 'A narrow permission.', h('br'), 'A clear boundary.'),
      h('div', { className: 'space-y-6' }, p('Sign-in happens with Google in your system browser. MirageSSD requests drive.file access to create and manage app-authorized files—not blanket access to all of My Drive. File contents travel between your PC and Google Drive, not through this website.'),
        p('The writable drive does not add client-side encryption to ordinary files. Local cached files and logs remain on your device. Separate encrypted repository and backup features are distinct from the Explorer drive.'), link('/privacy', 'Read the privacy policy'))),
    h('section', { className: 'rounded-3xl bg-ink px-7 py-10 text-white sm:px-10' },
      h('h2', { className: 'mb-4 text-2xl font-semibold tracking-tight' }, 'Try it with files you can afford to replace.'),
      h('p', { className: 'max-w-[65ch] leading-relaxed text-white/80' }, 'This is a Windows 11 x64 preview, not a certified backup product or a physical SSD replacement. Keep originals until you have verified the remote copy and a restore. Games, databases, and virtual machines should run on native storage.'),
      h('p', { className: 'mt-5 text-sm text-white/80' }, 'Requires internet, an NTFS volume with at least 12 GiB free, and permission to install WinFsp. macOS and Linux desktop mounting are not supported.')),
    h('section', { className: 'py-12' }, h('h2', { className: 'mb-6 text-2xl font-semibold tracking-tight' }, 'Before you connect'),
      ...[
        ['Does MirageSSD include Google Drive storage?', 'No. It uses the quota of the Google account you connect. It does not provide or increase your storage subscription.'],
        ['Can I access every existing file in My Drive?', 'No. The preview uses the drive.file scope and an application-owned folder. This is not a full mirror of your existing Drive.'],
        ['Is sign-in open to everyone yet?', 'Public OAuth setup is being completed. Until the Google project is published, sign-in can remain limited to approved test users.'],
        ['Where do I get an installer?', 'Follow the repository’s build and installation guide, or use a preview installer supplied by a maintainer. This website does not currently host an installer.']
      ].map(([question, answer]) => h('details', { key: question, className: 'border-t border-ink/15 py-5' }, h('summary', { className: 'cursor-pointer font-medium' }, question), h('div', { className: 'pt-4' }, p(answer))))));
}

function Legal({ title, intro, children }) {
  return h('article', { className: 'mx-auto max-w-3xl py-14 sm:py-20' },
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
for (const [filename, path, title, description, content] of pages) {
  await writeFile(`dist/${filename}`, '<!doctype html>' + renderToStaticMarkup(h(Layout, { title, description, path }, content)));
}
await writeFile('dist/robots.txt', `User-agent: *\nAllow: /\nSitemap: ${origin}/sitemap.xml\n`);
await writeFile('dist/sitemap.xml', `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${pages.slice(0, 3).map(([, path]) => `<url><loc>${origin}${path}</loc></url>`).join('')}</urlset>`);
console.log('Built homepage, privacy, terms, and 404 as static HTML. No client-side JavaScript.');

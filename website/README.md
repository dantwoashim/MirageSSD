# MirageSSD public website

Static homepage and OAuth policy pages. React renders HTML at build time; Tailwind generates local CSS. No browser JavaScript, analytics, forms, or Drive credentials are included.

```powershell
cd website
npm ci
npm run build
vercel deploy --prod
```

Vercel project: `miragessd-website`. If connecting the GitHub repository, set the project's Root Directory to `website` before enabling automatic builds. A direct CLI deployment does not require GitHub integration.

Production hostname: `miragessd.prabinghimire1.com.np`. Do not change the parent domain's existing records or nameservers; Project Parva uses them. Add only the subdomain CNAME supplied by `vercel domains verify miragessd.prabinghimire1.com.np`, with Cloudflare proxy disabled.

## Google OAuth publishing

After the custom hostname serves all pages publicly over HTTPS:

- Homepage: `https://miragessd.prabinghimire1.com.np/`
- Privacy policy: `https://miragessd.prabinghimire1.com.np/privacy`
- Terms: `https://miragessd.prabinghimire1.com.np/terms`
- Authorized domain: `prabinghimire1.com.np`

Verify domain ownership in Google Search Console using the OAuth project's authorized account and Google's exact TXT value. Add that TXT record; never replace unrelated TXT records. Vercel hostname verification is not Google domain verification. The maintainer reported production OAuth publishing on 5 September 2026; the homepage reflects that status, not a claim of Google brand verification.

Keep policy text consistent with shipped behavior. Do not describe ordinary writable-drive files as client-side encrypted or imply that the website receives Drive data.

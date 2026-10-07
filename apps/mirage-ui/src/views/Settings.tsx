import { ArrowSquareOut, DownloadSimple } from '@phosphor-icons/react';
import { useState } from 'react';
import type { DriveAccount } from '../api/account';
import type { ServiceClient } from '../api/client';
import { AccountChip } from '../components/AccountChip';
import { ThemeToggle } from '../components/ThemeToggle';
import { formatClock } from '../format';
import { ISSUES_URL, LICENSE_URL, PRIVACY_URL, REPO_URL } from '../links';
import type { UpdateCheck } from '../models';
import { Button, ButtonLink, Card, useToast } from '../ui';

export function SettingsView({
  client,
  account,
  updateInfo,
  unavailable,
  onChanged,
}: {
  client: ServiceClient;
  account: DriveAccount;
  updateInfo?: UpdateCheck;
  unavailable?: boolean;
  onChanged?: () => void;
}) {
  const toast = useToast();
  const [collecting, setCollecting] = useState(false);

  const collect = async () => {
    setCollecting(true);
    try {
      const path = await client.diagnosticsCollect();
      toast.success(`Diagnostics saved to ${path}`);
    } catch (failure) {
      toast.error(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setCollecting(false);
    }
  };

  const updateLine = updateInfo
    ? `MirageSSD ${updateInfo.current ?? ''} · ${updateInfo.channel ?? 'preview'} channel · last checked ${
        updateInfo.checked_at ? formatClock(new Date(updateInfo.checked_at * 1000)) : 'never'
      }`
    : 'Update status is unavailable.';

  const aboutLinks = [
    { label: 'Source code', href: REPO_URL },
    { label: 'Privacy policy', href: PRIVACY_URL },
    { label: 'License', href: LICENSE_URL },
    { label: 'Report a problem', href: ISSUES_URL },
  ];

  return (
    <div className="grid gap-4">
      <Card title="Google account" description="Signs your drives in to Google Drive.">
        <AccountChip account={account} onChanged={onChanged} unavailable={unavailable} />
      </Card>

      <Card title="Appearance">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-[13px] text-fg-muted">Match Windows, or pick a theme for this app.</p>
          <ThemeToggle />
        </div>
      </Card>

      <Card title="Updates" description={updateLine}>
        {updateInfo?.update_available && updateInfo.latest && updateInfo.url && (
          <ButtonLink href={updateInfo.url} icon={<DownloadSimple size={15} />}>
            Download {updateInfo.latest.replace(/^v/, '')}
          </ButtonLink>
        )}
      </Card>

      <Card
        title="Diagnostics"
        description="Creates a zip of recent MirageSSD logs on this PC. Review it before sharing — it can contain file paths."
      >
        <Button variant="secondary" loading={collecting} onClick={() => void collect()}>
          Collect diagnostics
        </Button>
      </Card>

      <Card title="About">
        <p className="text-[13px] text-fg-muted">Version {updateInfo?.current ?? 'unknown'}</p>
        <ul className="mt-3 flex flex-wrap gap-x-5 gap-y-1.5">
          {aboutLinks.map((link) => (
            <li key={link.href}>
              <a
                href={link.href}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 text-[13px] font-medium text-accent underline-offset-4 transition-colors duration-150 ease-standard hover:underline"
              >
                {link.label}
                <ArrowSquareOut size={12} aria-hidden="true" />
              </a>
            </li>
          ))}
        </ul>
      </Card>
    </div>
  );
}

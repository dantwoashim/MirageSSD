import { ArrowSquareOut } from '@phosphor-icons/react';
import { useState } from 'react';
import type { ServiceClient } from '../api/client';
import { ISSUES_URL, TROUBLESHOOTING_URL } from '../links';
import { Button, ButtonLink, Card, Notice, useToast } from '../ui';

const fixes: { title: string; fix: string }[] = [
  {
    title: 'The drive letter is missing',
    fix: "Open Drives and choose Connect. If it still doesn't appear, start the background service from this page, then sign out of Windows and back in. Don't format anything, delete the cache, or remove credentials as a first step.",
  },
  {
    title: 'Copies start fast, then slow down',
    fix: 'Saving to the drive finishes on this PC first; uploading to Google Drive then runs at your internet upload speed. Many small files take longer than one large file. The drive page shows what is still waiting to upload.',
  },
  {
    title: 'My local disk is filling up',
    fix: 'Use Free up space on the drive page to remove local copies that are already in Google Drive, or move the cache to a disk with more room. Files still waiting to upload stay on this PC until they finish.',
  },
  {
    title: 'Google sign-in expired or failed',
    fix: 'Open Settings and sign in again. Files already in Google Drive are not affected. Never share sign-in tokens in a support request.',
  },
  {
    title: 'The drive shows a different size than my disk',
    fix: 'Drive capacity comes from your Google storage quota. Local cache space and free disk space are separate amounts.',
  },
];

export function HelpView({
  client,
  serviceDown,
  onRefresh,
}: {
  client: ServiceClient;
  serviceDown: boolean;
  onRefresh: () => void;
}) {
  const toast = useToast();
  const [starting, setStarting] = useState(false);
  const [collecting, setCollecting] = useState(false);

  const startService = async () => {
    setStarting(true);
    try {
      await client.serviceStart();
      window.setTimeout(onRefresh, 3000);
    } catch (failure) {
      toast.error(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setStarting(false);
    }
  };

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

  return (
    <div className="grid gap-4">
      <Card title="Service status">
        {serviceDown ? (
          <Notice tone="warning" title="The MirageSSD service is not answering">
            <p>Start the service to bring your drives back. Windows may ask for permission.</p>
          </Notice>
        ) : (
          <p className="flex items-center gap-2 text-[13px] text-fg-muted" role="status">
            <span className="status-dot text-ok" aria-hidden="true" />
            The MirageSSD service is running.
          </p>
        )}
        <div className="mt-4 flex flex-wrap gap-2">
          {serviceDown && (
            <Button loading={starting} onClick={() => void startService()}>
              Start service
            </Button>
          )}
          <Button variant="secondary" loading={collecting} onClick={() => void collect()}>
            Collect diagnostics
          </Button>
        </div>
      </Card>

      <Card title="Common fixes">
        <ul className="grid gap-5">
          {fixes.map((fix) => (
            <li key={fix.title}>
              <h3 className="text-[13px] font-semibold text-fg">{fix.title}</h3>
              <p className="mt-1 max-w-[68ch] text-[13px] leading-relaxed text-fg-muted">{fix.fix}</p>
            </li>
          ))}
        </ul>
      </Card>

      <Card title="More help">
        <div className="flex flex-wrap gap-2">
          <ButtonLink variant="secondary" href={TROUBLESHOOTING_URL} icon={<ArrowSquareOut size={14} />}>
            Troubleshooting guide
          </ButtonLink>
          <ButtonLink variant="secondary" href={ISSUES_URL} icon={<ArrowSquareOut size={14} />}>
            Report a problem
          </ButtonLink>
        </div>
      </Card>
    </div>
  );
}

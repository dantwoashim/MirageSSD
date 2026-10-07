import { useCallback, useMemo, useRef, useState } from 'react';
import type { ServiceClient } from '../api/client';
import type { RepositoryState, ServiceSnapshot } from '../models';
import { repositoryScope } from '../presentation';
import { usePolling } from './usePolling';

export function message(value: unknown): string {
  return value instanceof Error
    ? value.message
    : 'MirageSSD could not complete this operation. Refresh its status before retrying.';
}

/**
 * The shared service-state loop: snapshot polling, the selected repository,
 * and its detail fetch — with request-sequence guards so stale responses
 * never overwrite fresher state, and an in-flight dedupe so overlapping
 * refreshes share one request.
 */
export function useServiceState(client: ServiceClient) {
  const [snapshot, setSnapshot] = useState<ServiceSnapshot | null>(null);
  const [selectedId, setSelectedId] = useState<string>();
  const selectedRef = useRef<string | undefined>(undefined);
  const [detail, setDetail] = useState<RepositoryState>();
  const [detailError, setDetailError] = useState<string>();
  const detailSequence = useRef(0);
  const [refreshing, setRefreshing] = useState(false);
  const refreshPromise = useRef<Promise<void> | null>(null);
  const [connectionError, setConnectionError] = useState<string>();
  const [lastChecked, setLastChecked] = useState<Date>();
  const operationLock = useRef(false);

  const loadDetail = useCallback(
    async (id: string) => {
      const sequence = ++detailSequence.current;
      try {
        const next = await client.detail(id);
        if (sequence === detailSequence.current && selectedRef.current === id) {
          setDetail(next);
          setDetailError(undefined);
        }
      } catch (error) {
        if (sequence === detailSequence.current && selectedRef.current === id) {
          setDetail(undefined);
          setDetailError(message(error));
        }
      }
    },
    [client],
  );

  const refresh = useCallback((): Promise<void> => {
    if (refreshPromise.current) return refreshPromise.current;
    setRefreshing(true);
    const request = (async () => {
      try {
        const next = await client.snapshot();
        setSnapshot(next);
        const id = next.repositories.some((item) => item.id === selectedRef.current)
          ? selectedRef.current
          : next.repositories[0]?.id;
        selectedRef.current = id;
        setSelectedId(id);
        setConnectionError(undefined);
        setLastChecked(new Date());
        if (id) await loadDetail(id);
        else setDetail(undefined);
      } catch (error) {
        setConnectionError(message(error));
      } finally {
        setRefreshing(false);
        refreshPromise.current = null;
      }
    })();
    refreshPromise.current = request;
    return request;
  }, [client, loadDetail]);

  usePolling(refresh, { intervalMs: 5_000 });

  const select = useCallback(
    (id: string) => {
      if (operationLock.current) return;
      selectedRef.current = id;
      setSelectedId(id);
      setDetail(undefined);
      setDetailError(undefined);
      void loadDetail(id);
    },
    [loadDetail],
  );

  const selected = useMemo(() => {
    const summary = snapshot?.repositories.find((item) => item.id === selectedId);
    return summary && detail?.id === summary.id && repositoryScope(detail) === repositoryScope(summary)
      ? { ...summary, ...detail }
      : summary;
  }, [snapshot, selectedId, detail]);

  return {
    snapshot,
    selectedId,
    selected,
    detailError,
    select,
    refresh,
    refreshing,
    connectionError,
    lastChecked,
    operationLock,
  };
}

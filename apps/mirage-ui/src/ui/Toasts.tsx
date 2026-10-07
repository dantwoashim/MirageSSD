import { CheckCircle, Info, WarningCircle, X } from '@phosphor-icons/react';
import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from 'react';
import { cx } from './cx';

type ToastKind = 'success' | 'error' | 'info';
type ToastItem = { id: number; kind: ToastKind; message: string; leaving?: boolean };

export type ToastApi = {
  success: (message: string) => void;
  error: (message: string) => void;
  info: (message: string) => void;
};

const noop: ToastApi = { success: () => {}, error: () => {}, info: () => {} };
const ToastContext = createContext<ToastApi>(noop);

export function useToast(): ToastApi {
  return useContext(ToastContext);
}

const icons: Record<ToastKind, typeof CheckCircle> = {
  success: CheckCircle,
  error: WarningCircle,
  info: Info,
};
const iconTones: Record<ToastKind, string> = {
  success: 'text-ok',
  error: 'text-danger',
  info: 'text-accent',
};

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const nextId = useRef(0);
  const leaving = useRef(new Set<number>());

  const dismiss = useCallback((id: number) => {
    if (leaving.current.has(id)) return;
    leaving.current.add(id);
    setToasts((list) => list.map((toast) => (toast.id === id ? { ...toast, leaving: true } : toast)));
    window.setTimeout(() => {
      leaving.current.delete(id);
      setToasts((list) => list.filter((toast) => toast.id !== id));
    }, 160);
  }, []);

  const push = useCallback(
    (kind: ToastKind, message: string) => {
      const id = ++nextId.current;
      setToasts((list) => [...list, { id, kind, message }]);
      if (kind !== 'error' && typeof window !== 'undefined') {
        window.setTimeout(() => dismiss(id), 5000);
      }
    },
    [dismiss],
  );

  const api = useMemo<ToastApi>(
    () => ({
      success: (message) => push('success', message),
      error: (message) => push('error', message),
      info: (message) => push('info', message),
    }),
    [push],
  );

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div className="fixed bottom-5 right-5 z-40 grid w-[min(360px,calc(100vw-2.5rem))] gap-2" aria-live="polite">
        {toasts.map((toast) => (
          <Toast key={toast.id} toast={toast} onDismiss={() => dismiss(toast.id)} />
        ))}
      </div>
    </ToastContext.Provider>
  );
}

function Toast({ toast, onDismiss }: { toast: ToastItem; onDismiss: () => void }) {
  const Icon = icons[toast.kind];
  return (
    <div
      role={toast.kind === 'error' ? 'alert' : 'status'}
      className={cx(
        'toast flex items-start gap-3 rounded-xl border border-line-strong bg-surface p-3.5 text-[13px] leading-relaxed shadow-lg',
        toast.leaving && 'leaving',
      )}
    >
      <Icon size={18} weight="bold" className={cx('mt-0.5 shrink-0', iconTones[toast.kind])} aria-hidden="true" />
      <p className="min-w-0 flex-1 text-fg">{toast.message}</p>
      {toast.kind === 'error' ? (
        <span className="flex shrink-0 items-center gap-1.5">
          <CopyDetails message={toast.message} />
          <button
            type="button"
            onClick={onDismiss}
            className="rounded-md px-2 py-1 text-xs font-medium text-fg-muted transition-colors duration-150 ease-standard hover:bg-surface-2 hover:text-fg"
          >
            Dismiss
          </button>
        </span>
      ) : (
        <button
          type="button"
          aria-label="Dismiss"
          onClick={onDismiss}
          className="shrink-0 rounded-md p-1 text-fg-muted transition-colors duration-150 ease-standard hover:bg-surface-2 hover:text-fg"
        >
          <X size={15} aria-hidden="true" />
        </button>
      )}
    </div>
  );
}

function CopyDetails({ message }: { message: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      onClick={() => {
        void navigator.clipboard
          ?.writeText(message)
          .then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1500);
          })
          .catch(() => {});
      }}
      className="rounded-md px-2 py-1 text-xs font-medium text-fg-muted transition-colors duration-150 ease-standard hover:bg-surface-2 hover:text-fg"
    >
      {copied ? 'Copied' : 'Copy details'}
    </button>
  );
}

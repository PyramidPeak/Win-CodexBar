import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { GrokAccount, GrokAccountUsage } from "../types/bridge";
import { grokAccountsList, grokAccountSwitch, grokAccountFetch } from "../lib/tauri";
import { useLocale } from "../hooks/useLocale";
import { useFormattedResetTime } from "../hooks/useFormattedResetTime";
import { maskEmail } from "./MenuCard";

export default function GrokAccountsMenu({
  hideEmail,
  resetTimeRelative,
  onLayoutChange,
}: {
  hideEmail: boolean;
  resetTimeRelative: boolean;
  onLayoutChange?: () => void;
}) {
  const { t } = useLocale();
  const [accounts, setAccounts] = useState<GrokAccount[]>([]);
  const [usage, setUsage] = useState<Record<string, GrokAccountUsage>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [switched, setSwitched] = useState(false);
  const mounted = useRef(false);
  const load = useCallback(async () => {
    const next = await grokAccountsList();
    if (mounted.current) {
      setAccounts(next);
      setError(null);
    }
    const snapshots: Record<string, GrokAccountUsage> = {};
    await Promise.all(
      next.map(async (account) => {
        try {
          snapshots[account.id] = await grokAccountFetch(account.id);
        } catch {
          // Keep the row even if usage is unavailable.
        }
      }),
    );
    if (mounted.current) setUsage(snapshots);
  }, []);
  useEffect(() => {
    mounted.current = true;
    const reload = () => {
      void load().catch((e) => {
        if (mounted.current) setError(String(e));
      });
    };
    reload();
    window.addEventListener("focus", reload);
    const unlisten = listen("grok-accounts-updated", reload);
    return () => {
      mounted.current = false;
      window.removeEventListener("focus", reload);
      void unlisten.then((fn) => fn()).catch(() => {});
    };
  }, [load]);
  useEffect(() => {
    onLayoutChange?.();
  }, [accounts.length, error, switched, onLayoutChange]);

  const switchAccount = async (id: string) => {
    setBusy(true);
    setError(null);
    setSwitched(false);
    try {
      await grokAccountSwitch(id);
      await load();
      if (mounted.current) setSwitched(true);
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  const hasSwitchableAccount = accounts.some(
    (account) => account.isSaved && !account.isActive,
  );
  if (accounts.length <= 1 && !hasSwitchableAccount && !error) return null;
  return (
    <details className="codex-menu-accounts" onToggle={onLayoutChange}>
      <summary className="codex-menu-accounts__summary">
        <span className="codex-menu-accounts__title">{t("GrokAccountsTitle")}</span>
        <span className="codex-menu-accounts__count">{accounts.length}</span>
      </summary>
      {error && (
        <div className="codex-menu-accounts__error" role="alert">
          {error}
        </div>
      )}
      {switched && <p role="status">{t("GrokAccountsSwitched")}</p>}
      <ul className="codex-menu-accounts__list">
        {accounts.map((account) => (
          <GrokAccountRow
            key={account.id}
            account={account}
            snapshot={usage[account.id]}
            hideEmail={hideEmail}
            resetTimeRelative={resetTimeRelative}
            busy={busy}
            onSwitch={switchAccount}
          />
        ))}
      </ul>
    </details>
  );
}

function GrokAccountRow({
  account,
  snapshot,
  hideEmail,
  resetTimeRelative,
  busy,
  onSwitch,
}: {
  account: GrokAccount;
  snapshot: GrokAccountUsage | undefined;
  hideEmail: boolean;
  resetTimeRelative: boolean;
  busy: boolean;
  onSwitch: (id: string) => Promise<void>;
}) {
  const { t } = useLocale();
  const email = hideEmail ? maskEmail(account.email) : account.email;
  const pct = snapshot?.usedPercent != null ? Math.round(snapshot.usedPercent) : null;
  const resetText = useFormattedResetTime(
    snapshot?.resetsAt ?? null,
    null,
    resetTimeRelative,
  );
  const resetLabel = resetText
    ? resetTimeRelative
      ? resetText
      : `${t("MetricResetsIn")} ${resetText}`
    : null;
  const windowLabel = formatWindowLabel(snapshot?.windowMinutes);
  const barLevel =
    pct != null && pct >= 100 ? "exhausted" : pct != null && pct >= 90 ? "critical" : undefined;

  return (
    <li>
      <div
        className={`codex-menu-accounts__row${account.isActive ? " codex-menu-accounts__row--active" : ""}`}
      >
        <div className="codex-menu-accounts__meta">
          <span className="codex-menu-accounts__email" title={email}>
            {email}
            {account.isActive && (
              <span className="codex-menu-accounts__badge">
                {t("TokenAccountActive")}
              </span>
            )}
          </span>
          {(pct !== null || resetLabel) && (
            <span className="codex-menu-accounts__usage">
              {windowLabel && <span>{windowLabel}</span>}
              {pct !== null && <span>{pct}% {t("PanelUsedSuffix")}</span>}
              {resetLabel && <span>{resetLabel}</span>}
            </span>
          )}
          {pct !== null && (
            <span className="codex-menu-accounts__bar" aria-hidden>
              <span
                className="codex-menu-accounts__bar-fill"
                data-level={barLevel}
                style={{ width: `${Math.max(2, Math.min(100, pct))}%` }}
              />
            </span>
          )}
        </div>
        <button
          type="button"
          className="codex-menu-accounts__switch"
          disabled={busy || account.isActive || !account.isSaved}
          onClick={() => void onSwitch(account.id)}
        >
          {t("CodexAccountsSwitchButton")}
        </button>
      </div>
    </li>
  );
}

function formatWindowLabel(windowMinutes: number | null | undefined): string | null {
  if (!windowMinutes || windowMinutes <= 0) return null;
  if (windowMinutes % 1_440 === 0) return `${windowMinutes / 1_440}d`;
  if (windowMinutes % 60 === 0) return `${windowMinutes / 60}h`;
  return null;
}

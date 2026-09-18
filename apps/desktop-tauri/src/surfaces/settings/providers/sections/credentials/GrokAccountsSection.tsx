import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { GrokAccount, GrokAccountUsage } from "../../../../../types/bridge";
import type { LocaleKey } from "../../../../../i18n/keys";
import {
  grokAccountsList,
  grokAccountAdd,
  grokAccountCancelLogin,
  grokAccountSaveCurrent,
  grokAccountRemove,
  grokAccountSwitch,
  grokAccountFetch,
} from "../../../../../lib/tauri";

export function GrokAccountsSection({ t }: { t: (key: LocaleKey) => string }) {
  const [accounts, setAccounts] = useState<GrokAccount[]>([]);
  const [usage, setUsage] = useState<Record<string, GrokAccountUsage>>({});
  const [busy, setBusy] = useState(false);
  const [loggingIn, setLoggingIn] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const mounted = useRef(false);
  const load = useCallback(async () => {
    const next = await grokAccountsList();
    if (mounted.current) setAccounts(next);
    const snapshots: Record<string, GrokAccountUsage> = {};
    await Promise.all(
      next.map(async (account) => {
        try {
          snapshots[account.id] = await grokAccountFetch(account.id);
        } catch {
          // Keep the account row even if that login's usage fetch fails.
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
    const unlisten = listen("grok-accounts-updated", reload);
    return () => {
      mounted.current = false;
      void unlisten.then((fn) => fn());
    };
  }, [load]);
  const run = async (operation: () => Promise<void>, success?: LocaleKey) => {
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await operation();
      await load();
      if (mounted.current && success) setMessage(t(success));
    } catch (e) {
      if (mounted.current) setError(String(e));
    } finally {
      if (mounted.current) {
        setBusy(false);
        setLoggingIn(false);
      }
    }
  };
  return (
    <section className="provider-detail-section codex-accounts">
      <h4>{t("GrokAccountsTitle")}</h4>
      <p className="settings-section__hint">{t("GrokAccountsHint")}</p>
      {error && (
        <div className="provider-detail-error" role="alert">
          {error}
        </div>
      )}
      {message && (
        <div className="provider-detail-note" role="status">
          {message}
        </div>
      )}
      {loggingIn && <p role="status">{t("GrokAccountsSigningIn")}</p>}
      {accounts.length === 0 && <p>{t("GrokAccountsEmpty")}</p>}
      <ul className="credential-list">
        {accounts.map((account) => (
          <li className="credential-card" key={account.id}>
            <div className="credential-card__header">
              <div className="credential-card__info">
                <strong>{account.email}</strong>
                <span className="credential-card__meta">
                  {usageLabel(account, usage[account.id])}
                </span>
                {account.isActive && (
                  <span className="credential-card__badge credential-card__badge--set">
                    {t("TokenAccountActive")}
                  </span>
                )}
              </div>
              <div className="credential-card__actions">
                {!account.isActive && account.isSaved && (
                  <button
                    className="credential-btn credential-btn--primary"
                    disabled={busy}
                    onClick={() =>
                      void run(
                        () => grokAccountSwitch(account.id),
                        "GrokAccountsSwitched",
                      )
                    }
                  >
                    {t("CodexAccountsSwitchButton")}
                  </button>
                )}
                {!account.isSaved && (
                  <button
                    className="credential-btn credential-btn--secondary"
                    disabled={busy}
                    onClick={() => void run(grokAccountSaveCurrent)}
                  >
                    {t("GrokAccountsSaveCurrent")}
                  </button>
                )}
                {account.isSaved && (
                  <button
                    className="credential-btn credential-btn--danger"
                    disabled={busy}
                    onClick={() => void run(() => grokAccountRemove(account.id))}
                  >
                    {t("CodexAccountsRemoveButton")}
                  </button>
                )}
              </div>
            </div>
          </li>
        ))}
      </ul>
      <button
        className="credential-btn credential-btn--primary"
        disabled={busy}
        onClick={() => {
          setLoggingIn(true);
          void run(grokAccountAdd, "GrokAccountsAdded");
        }}
      >
        {t("CodexAccountsAddButton")}
      </button>
      {loggingIn && (
        <button
          className="credential-btn credential-btn--secondary"
          onClick={() =>
            void grokAccountCancelLogin().catch((e) => setError(String(e)))
          }
        >
          {t("GrokAccountsCancelLogin")}
        </button>
      )}
    </section>
  );
}

function usageLabel(account: GrokAccount, snapshot?: GrokAccountUsage): string {
  const plan = snapshot?.plan || account.plan || "";
  const percent =
    snapshot?.usedPercent != null ? `${Math.round(snapshot.usedPercent)}%` : null;
  return [plan, percent].filter(Boolean).join(" · ");
}

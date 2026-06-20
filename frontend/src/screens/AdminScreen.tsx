import { useMemo, useState } from "react";
import type { JSX } from "react";
import { useTranslation } from "react-i18next";

import type { FeedSnapshot } from "../types";
import type { WalletAdapter } from "../wallet";
import { IndexerPanel } from "../panels/IndexerPanel";
import { KeeperPanel } from "../panels/KeeperPanel";
import { LeaderLockBanner } from "../panels/LeaderLockBanner";
import { LeaderLockGrid } from "../panels/LeaderLockGrid";
import { MarketSelector } from "../panels/MarketSelector";
import { decodeKeeperLeaderLockBytes } from "../tx/wasmBuilder";
import type { KeeperLeaderLockView } from "../tx/wasmBuilder";
import { AppHeader } from "./AppHeader";
import "./screens.css";

const ADMIN_USER = "admin";
const ADMIN_PASS = "admin123";
const SESSION_KEY = "mole_admin_authed";

interface Props {
  feed: FeedSnapshot | null | undefined;
  status: string;
  expectedLeaders?: Map<string, string>;
  wallet: WalletAdapter;
  symbols: string[];
  activeSymbol: string | null;
  onSymbolChange: (s: string) => void;
  onHome: () => void;
}

type AdminTab = "indexer" | "keeper";

function isAuthed(): boolean {
  try {
    return sessionStorage.getItem(SESSION_KEY) === "1";
  } catch {
    return false;
  }
}

export function AdminScreen({
  feed,
  status,
  expectedLeaders,
  wallet,
  symbols,
  activeSymbol,
  onSymbolChange,
  onHome,
}: Props): JSX.Element {
  const { t } = useTranslation();
  const [authed, setAuthed] = useState<boolean>(isAuthed());
  const [tab, setTab] = useState<AdminTab>("indexer");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [error, setError] = useState(false);

  const leaderLockView = useMemo<KeeperLeaderLockView | null>(() => {
    const bytes = feed?.keeperLeaderLockBytes;
    if (!bytes || bytes.length === 0) return null;
    try {
      return decodeKeeperLeaderLockBytes(bytes);
    } catch {
      return null;
    }
  }, [feed?.keeperLeaderLockBytes]);

  function submit(e: React.FormEvent) {
    e.preventDefault();
    if (user === ADMIN_USER && pass === ADMIN_PASS) {
      try {
        sessionStorage.setItem(SESSION_KEY, "1");
      } catch {
        /* ignore storage failures */
      }
      setAuthed(true);
      setError(false);
    } else {
      setError(true);
    }
  }

  function logout() {
    try {
      sessionStorage.removeItem(SESSION_KEY);
    } catch {
      /* ignore */
    }
    setAuthed(false);
    setUser("");
    setPass("");
  }

  if (!authed) {
    return (
      <div className="sc-page sc-login-page">
        <form className="sc-login" onSubmit={submit}>
          <div className="sc-login-brand">
            <span className="sc-brand-mark" /> {t("common.appName")}
          </div>
          <h1>{t("admin.loginTitle")}</h1>
          <p className="sc-login-sub">{t("admin.loginSub")}</p>
          <label>
            <span>{t("admin.username")}</span>
            <input
              value={user}
              autoComplete="username"
              onChange={(e) => setUser(e.target.value)}
            />
          </label>
          <label>
            <span>{t("admin.password")}</span>
            <input
              type="password"
              value={pass}
              autoComplete="current-password"
              onChange={(e) => setPass(e.target.value)}
            />
          </label>
          {error && <div className="sc-login-error">{t("admin.wrongCreds")}</div>}
          <button type="submit" className="sc-login-btn">
            {t("admin.signIn")}
          </button>
          <button type="button" className="sc-login-back" onClick={onHome}>
            ← {t("common.home")}
          </button>
        </form>
      </div>
    );
  }

  return (
    <div className="sc-page">
      <AppHeader variant="admin" onHome={onHome} onLogout={logout} />
      <main className="sc-main">
        <div className="sc-title-row">
          <div>
            <h1 className="sc-title">{t("admin.badge")}</h1>
            <p className="sc-subtitle">{t("admin.restricted")}</p>
          </div>
        </div>

        {!feed ? (
          <div className="sc-loading">{status}…</div>
        ) : (
          <>
            {feed.marketsView && symbols.length > 0 ? (
              <MarketSelector
                symbols={symbols}
                active={activeSymbol ?? symbols[0] ?? ""}
                onChange={onSymbolChange}
                view={feed.marketsView}
                positions={feed.positions}
                {...(feed.currentSlot !== undefined && {
                  currentSlot: feed.currentSlot,
                })}
              />
            ) : null}

            {feed.marketsView ? (
              <LeaderLockGrid
                view={feed.marketsView}
                currentSlot={feed.currentSlot ?? BigInt(feed.indexer.slot)}
                decode={decodeKeeperLeaderLockBytes}
                {...(expectedLeaders ? { expected: expectedLeaders } : {})}
              />
            ) : (
              <LeaderLockBanner
                view={leaderLockView}
                currentSlot={feed.currentSlot ?? BigInt(feed.indexer.slot)}
              />
            )}

            <nav className="sc-tabs">
              <button
                type="button"
                className={tab === "indexer" ? "active" : ""}
                onClick={() => setTab("indexer")}
              >
                {t("admin.indexer")}
              </button>
              <button
                type="button"
                className={tab === "keeper" ? "active" : ""}
                onClick={() => setTab("keeper")}
              >
                {t("admin.keeper")}
              </button>
            </nav>

            <div className="sc-panel-host">
              {tab === "indexer" ? (
                <IndexerPanel feed={feed} />
              ) : (
                <KeeperPanel feed={feed} wallet={wallet} />
              )}
            </div>
          </>
        )}
      </main>
    </div>
  );
}

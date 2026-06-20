import type { JSX } from "react";
import { useTranslation } from "react-i18next";
import { LanguageSwitcher } from "../i18n/LanguageSwitcher";
import { formatPubkey } from "../format";
import "./screens.css";

interface Props {
  variant: "dashboard" | "admin";
  walletStatus?: string;
  walletPubkeyHex?: string;
  onConnect?: () => void;
  onDisconnect?: () => void;
  onHome: () => void;
  onTrade?: () => void;
  onLogout?: () => void;
}

export function AppHeader({
  variant,
  walletStatus,
  walletPubkeyHex,
  onConnect,
  onDisconnect,
  onHome,
  onTrade,
  onLogout,
}: Props): JSX.Element {
  const { t } = useTranslation();
  const connected = walletStatus === "connected";

  return (
    <header className="sc-header">
      <button type="button" className="sc-brand" onClick={onHome}>
        <span className="sc-brand-mark" /> {t("common.appName")}
      </button>

      <nav className="sc-nav">
        {onTrade && (
          <button type="button" className="sc-nav-link" onClick={onTrade}>
            {t("common.trade")}
          </button>
        )}
        <span
          className={`sc-nav-link ${variant === "dashboard" ? "active" : ""}`}
          aria-current={variant === "dashboard" ? "page" : undefined}
        >
          {t("common.dashboard")}
        </span>
        {variant === "admin" && (
          <span className="sc-nav-link active">{t("admin.badge")}</span>
        )}
      </nav>

      <div className="sc-header-right">
        <LanguageSwitcher variant="compact" />
        {variant === "admin" ? (
          <button type="button" className="sc-wallet" onClick={onLogout}>
            {t("admin.logout")}
          </button>
        ) : connected ? (
          <button type="button" className="sc-wallet connected" onClick={onDisconnect}>
            {walletPubkeyHex ? formatPubkey(walletPubkeyHex) : t("common.connected")} ·{" "}
            {t("common.disconnect")}
          </button>
        ) : (
          <button
            type="button"
            className="sc-wallet"
            onClick={onConnect}
            disabled={walletStatus === "connecting"}
          >
            {walletStatus === "connecting"
              ? t("common.connecting")
              : t("common.connectWallet")}
          </button>
        )}
      </div>
    </header>
  );
}

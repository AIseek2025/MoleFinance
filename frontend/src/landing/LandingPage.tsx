import type { JSX } from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { LanguageSwitcher } from "../i18n/LanguageSwitcher";
import "./landing.css";

interface Props {
  onLaunch: () => void;
  onConsole: () => void;
  livePriceUsd?: number | null;
  liveSymbol?: string;
}

interface Feature {
  tag: string;
  title: string;
  body: string;
}
interface Step {
  title: string;
  body: string;
}

export function LandingPage({
  onLaunch,
  onConsole,
  livePriceUsd,
  liveSymbol = "SOL-PERP",
}: Props): JSX.Element {
  const { t } = useTranslation();
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 12);
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

  const features = t("landing.features", { returnObjects: true }) as Feature[];
  const tradItems = t("landing.tradItems", { returnObjects: true }) as string[];
  const moleItems = t("landing.moleItems", { returnObjects: true }) as string[];
  const steps = t("landing.steps", { returnObjects: true }) as Step[];
  const riskItems = t("landing.riskItems", { returnObjects: true }) as string[];

  const priceLabel =
    livePriceUsd != null && Number.isFinite(livePriceUsd)
      ? `$${livePriceUsd.toLocaleString("en-US", {
          minimumFractionDigits: 2,
          maximumFractionDigits: 2,
        })}`
      : "—";

  return (
    <div className="lp">
      <div className="lp-aurora" aria-hidden />
      <div className="lp-grid-overlay" aria-hidden />

      <header className={`lp-nav ${scrolled ? "is-scrolled" : ""}`}>
        <div className="lp-nav-inner">
          <div className="lp-logo">
            <span className="lp-logo-mark" />
            {t("common.appName")}
          </div>
          <nav className="lp-nav-links">
            <a href="#features">{t("landing.navMechanism")}</a>
            <a href="#how">{t("landing.navFlow")}</a>
            <a href="#liquidation">{t("landing.navLiq")}</a>
            <a href="#risk">{t("landing.navRisk")}</a>
            <button type="button" className="lp-link-btn" onClick={onConsole}>
              {t("common.dashboard")}
            </button>
          </nav>
          <div className="lp-nav-right">
            <LanguageSwitcher variant="compact" />
            <button type="button" className="lp-cta-sm" onClick={onLaunch}>
              {t("common.launchApp")}
            </button>
          </div>
        </div>
      </header>

      <section className="lp-hero">
        <div className="lp-pill">
          <span className="lp-pill-dot" /> {t("landing.badge")}
        </div>
        <h1 className="lp-hero-title">
          {t("landing.heroTitle1")}
          <br />
          <span className="lp-grad">{t("landing.heroTitle2")}</span>
        </h1>
        <p className="lp-hero-sub">{t("landing.heroSub")}</p>
        <div className="lp-hero-actions">
          <button type="button" className="lp-cta" onClick={onLaunch}>
            {t("landing.ctaStart")}
          </button>
          <button type="button" className="lp-ghost" onClick={onConsole}>
            {t("landing.ctaData")}
          </button>
        </div>

        <div className="lp-ticker">
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">
              {t("landing.tickerPrice", { symbol: liveSymbol })}
            </span>
            <span className="lp-ticker-value lp-mono">{priceLabel}</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">{t("landing.tickerLiq")}</span>
            <span className="lp-ticker-value lp-accent">{t("landing.tickerLiqVal")}</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">{t("landing.tickerMaxLoss")}</span>
            <span className="lp-ticker-value">{t("landing.tickerMaxLossVal")}</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">{t("landing.tickerSettle")}</span>
            <span className="lp-ticker-value">{t("landing.tickerSettleVal")}</span>
          </div>
        </div>
      </section>

      <section className="lp-section" id="features">
        <div className="lp-section-head">
          <span className="lp-eyebrow">{t("landing.featuresEyebrow")}</span>
          <h2>{t("landing.featuresTitle")}</h2>
          <p>{t("landing.featuresSub")}</p>
        </div>
        <div className="lp-feature-grid">
          {features.map((f) => (
            <article key={f.title} className="lp-feature">
              <span className="lp-feature-tag">{f.tag}</span>
              <h3>{f.title}</h3>
              <p>{f.body}</p>
            </article>
          ))}
        </div>
      </section>

      <section className="lp-section lp-compare" id="liquidation">
        <div className="lp-section-head">
          <span className="lp-eyebrow">{t("landing.compareEyebrow")}</span>
          <h2>{t("landing.compareTitle")}</h2>
        </div>
        <div className="lp-compare-grid">
          <div className="lp-compare-col lp-bad">
            <h4>{t("landing.tradTitle")}</h4>
            <ul>
              {tradItems.map((it) => (
                <li key={it}>{it}</li>
              ))}
            </ul>
          </div>
          <div className="lp-compare-col lp-good">
            <h4>{t("landing.moleTitle")}</h4>
            <ul>
              {moleItems.map((it) => (
                <li key={it}>{it}</li>
              ))}
            </ul>
          </div>
        </div>
        <p className="lp-disclaimer-inline">{t("landing.compareDisclaimer")}</p>
      </section>

      <section className="lp-section" id="how">
        <div className="lp-section-head">
          <span className="lp-eyebrow">{t("landing.howEyebrow")}</span>
          <h2>{t("landing.howTitle")}</h2>
        </div>
        <div className="lp-steps">
          {steps.map((s, i) => (
            <div key={s.title} className="lp-step">
              <span className="lp-step-n">{String(i + 1).padStart(2, "0")}</span>
              <h3>{s.title}</h3>
              <p>{s.body}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="lp-section lp-risk" id="risk">
        <div className="lp-risk-card">
          <h3>{t("landing.riskTitle")}</h3>
          <ul>
            {riskItems.map((it) => (
              <li key={it}>{it}</li>
            ))}
          </ul>
          <p className="lp-risk-foot">{t("landing.riskFoot")}</p>
        </div>
      </section>

      <section className="lp-final">
        <h2>{t("landing.finalTitle")}</h2>
        <p>{t("landing.finalSub")}</p>
        <button type="button" className="lp-cta lp-cta-lg" onClick={onLaunch}>
          {t("landing.finalCta")}
        </button>
      </section>

      <footer className="lp-footer">
        <div className="lp-logo">
          <span className="lp-logo-mark" />
          {t("common.appName")}
        </div>
        <span className="lp-footer-note">{t("landing.footerNote")}</span>
      </footer>
    </div>
  );
}

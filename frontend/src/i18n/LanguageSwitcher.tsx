import type { JSX } from "react";
import { useTranslation } from "react-i18next";
import { SUPPORTED_LANGS } from "./index";

interface Props {
  /** Visual variant: light (on dark hero) or compact (in dense toolbar). */
  variant?: "default" | "compact";
}

export function LanguageSwitcher({ variant = "default" }: Props): JSX.Element {
  const { t, i18n } = useTranslation();
  const current =
    SUPPORTED_LANGS.find((l) => l.code === i18n.language)?.code ??
    (i18n.language?.startsWith("zh-Hant") ? "zh-Hant" : "en");

  return (
    <select
      className={`lang-switcher ${variant === "compact" ? "lang-compact" : ""}`}
      aria-label={t("lang.label")}
      value={current}
      onChange={(e) => void i18n.changeLanguage(e.target.value)}
    >
      {SUPPORTED_LANGS.map((l) => (
        <option key={l.code} value={l.code}>
          {t(`lang.${l.key}`)}
        </option>
      ))}
    </select>
  );
}

// i18n bootstrap. Default language is English; once a user picks a
// language it persists in localStorage. We deliberately do NOT auto-pick
// from navigator on first visit so the default is always English per
// product requirement.

import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import { en } from "./locales/en";
import { zhHans } from "./locales/zh-Hans";
import { zhHant } from "./locales/zh-Hant";
import { ja } from "./locales/ja";
import { ko } from "./locales/ko";
import { vi } from "./locales/vi";

export const SUPPORTED_LANGS = [
  { code: "en", key: "en" as const },
  { code: "zh-Hans", key: "zhHans" as const },
  { code: "zh-Hant", key: "zhHant" as const },
  { code: "ja", key: "ja" as const },
  { code: "ko", key: "ko" as const },
  { code: "vi", key: "vi" as const },
];

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      en: { translation: en },
      "zh-Hans": { translation: zhHans },
      "zh-Hant": { translation: zhHant },
      ja: { translation: ja },
      ko: { translation: ko },
      vi: { translation: vi },
    },
    fallbackLng: "en",
    supportedLngs: ["en", "zh-Hans", "zh-Hant", "ja", "ko", "vi"],
    // Default to English on first visit; only honor an explicit prior
    // choice persisted to localStorage.
    detection: {
      order: ["localStorage"],
      // Key is versioned: bumping it invalidates any previously persisted
      // choice so every client deterministically starts in English (the
      // product default) until the user explicitly picks a language again.
      lookupLocalStorage: "mole_lang_v1",
      caches: ["localStorage"],
    },
    interpolation: { escapeValue: false },
    returnObjects: true,
  });

export default i18n;

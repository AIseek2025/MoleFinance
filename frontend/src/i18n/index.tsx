import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import type { JSX, ReactNode } from "react";
import { TRANSLATIONS } from "./translations";

export type Lang = "en" | "ko" | "ja" | "vi" | "zh-Hant" | "zh-Hans";

export const LANGS: { code: Lang; label: string; short: string }[] = [
  { code: "en", label: "English", short: "EN" },
  { code: "ko", label: "한국어", short: "KO" },
  { code: "ja", label: "日本語", short: "JA" },
  { code: "vi", label: "Tiếng Việt", short: "VI" },
  { code: "zh-Hant", label: "繁體中文", short: "繁" },
  { code: "zh-Hans", label: "简体中文", short: "简" },
];

const STORAGE_KEY = "mole.lang";
const DEFAULT_LANG: Lang = "en";

function detectInitial(): Lang {
  if (typeof window === "undefined") return DEFAULT_LANG;
  const fromUrl = new URLSearchParams(window.location.search).get("lang");
  if (fromUrl && LANGS.some((l) => l.code === fromUrl)) return fromUrl as Lang;
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored && LANGS.some((l) => l.code === stored)) return stored as Lang;
  return DEFAULT_LANG;
}

type Vars = Record<string, string | number>;
export type TFn = (key: string, vars?: Vars) => string;

interface LangCtx {
  lang: Lang;
  setLang: (l: Lang) => void;
  t: TFn;
}

const Ctx = createContext<LangCtx | null>(null);

function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (m, k) =>
    k in vars ? String(vars[k]) : m,
  );
}

export function LanguageProvider({ children }: { children: ReactNode }): JSX.Element {
  const [lang, setLangState] = useState<Lang>(detectInitial);

  const setLang = useCallback((l: Lang) => {
    setLangState(l);
    if (typeof window !== "undefined") {
      window.localStorage.setItem(STORAGE_KEY, l);
      document.documentElement.lang = l;
    }
  }, []);

  useEffect(() => {
    if (typeof document !== "undefined") document.documentElement.lang = lang;
  }, [lang]);

  const t = useCallback<TFn>(
    (key, vars) => {
      const table = TRANSLATIONS[lang] ?? TRANSLATIONS.en;
      const raw = table[key] ?? TRANSLATIONS.en[key] ?? key;
      return interpolate(raw, vars);
    },
    [lang],
  );

  const value = useMemo<LangCtx>(() => ({ lang, setLang, t }), [lang, setLang, t]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useI18n(): LangCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useI18n must be used within LanguageProvider");
  return c;
}

export function useT(): TFn {
  return useI18n().t;
}

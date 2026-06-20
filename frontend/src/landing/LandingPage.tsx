import type { JSX } from "react";
import { useEffect, useState } from "react";
import "./landing.css";

interface Props {
  /** Jump into the Hyperliquid-style trading terminal. */
  onLaunch: () => void;
  /** Jump into the ops console (overview / indexer / keeper). */
  onConsole: () => void;
  /** Live mid price in USD (from the feed) for the hero ticker, if any. */
  livePriceUsd?: number | null;
  /** Symbol of the live market, e.g. "SOL-PERP". */
  liveSymbol?: string;
}

interface Feature {
  tag: string;
  title: string;
  body: string;
}

const FEATURES: Feature[] = [
  {
    tag: "核心创新",
    title: "永不爆仓",
    body: "没有维持保证金线，没有强平引擎。价格再剧烈波动，仓位也不会被清算机器人在最低点强制平掉。你的最大亏损被锁定为本金加开仓费用——仅此而已。",
  },
  {
    tag: "零和透明",
    title: "对手盘即清算池",
    body: "盈利来自对手盘的亏损，全部在链上可验证。没有暗箱保险基金常态补贴，没有做市商吃单——一个纯粹、可审计的零和市场。",
  },
  {
    tag: "权益可复活",
    title: "归零不等于出局",
    body: "当前权益为 0 不代表仓位被删除。价格反转、出现新的对手盘亏损时,你的仓位可以重新获得可提取权益。Shares 模型让你留在牌桌上。",
  },
  {
    tag: "风险隔离",
    title: "杠杆分场",
    body: "每个杠杆是独立的 Market,不共享资金池、不共享重置价格、不共享盈亏清算。10x 用户永远不会被 100x 用户的爆发式波动牵连。",
  },
  {
    tag: "即时结算",
    title: "当前区块清算",
    body: "Keeper 在每个区块用预言机价格执行 sync_pool,盈亏即时在子池间迁移。没有资金费率累积,没有跨期穿仓风险。",
  },
  {
    tag: "价格安全",
    title: "预言机封套保护",
    body: "每笔开/平仓都带 expected_price_min/max 封套。预言机过期、置信度过宽或价格跳变超阈值时,合约直接拒绝成交,把你挡在坏价之外。",
  },
];

const STEPS: { n: string; title: string; body: string }[] = [
  {
    n: "01",
    title: "选择市场与方向",
    body: "挑一个杠杆分场(如 SOL-PERP 10x),做多或做空,投入本金作为保证金。",
  },
  {
    n: "02",
    title: "仓位进入子池",
    body: "你的本金按当前预言机价格铸造 shares,进入对应方向的子池,与对手盘对冲。",
  },
  {
    n: "03",
    title: "Keeper 逐块清算",
    body: "每个区块按真实价格迁移盈亏。盈利方从亏损方的本金中按 shares 比例兑现。",
  },
  {
    n: "04",
    title: "随时平仓提取",
    body: "平仓即按当前权益结算可提取金额。即使曾归零,价格回来仍可能复活权益。",
  },
];

export function LandingPage({
  onLaunch,
  onConsole,
  livePriceUsd,
  liveSymbol = "SOL-PERP",
}: Props): JSX.Element {
  const [scrolled, setScrolled] = useState(false);
  useEffect(() => {
    const onScroll = () => setScrolled(window.scrollY > 12);
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => window.removeEventListener("scroll", onScroll);
  }, []);

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
            MoleOption
          </div>
          <nav className="lp-nav-links">
            <a href="#features">机制</a>
            <a href="#how">流程</a>
            <a href="#liquidation">永不爆仓</a>
            <a href="#risk">风险</a>
            <button type="button" className="lp-link-btn" onClick={onConsole}>
              运维控制台
            </button>
          </nav>
          <button type="button" className="lp-cta-sm" onClick={onLaunch}>
            进入交易
          </button>
        </div>
      </header>

      <section className="lp-hero">
        <div className="lp-pill">
          <span className="lp-pill-dot" /> 已部署 Solana Devnet · 实时清算运行中
        </div>
        <h1 className="lp-hero-title">
          永不爆仓的
          <br />
          <span className="lp-grad">链上杠杆交易</span>
        </h1>
        <p className="lp-hero-sub">
          MoleOption 用 shares 资金池与逐块清算重写杠杆交易规则。没有强平引擎，
          没有维持保证金线——你的最大亏损永远不超过本金，权益归零后仍有机会复活。
        </p>
        <div className="lp-hero-actions">
          <button type="button" className="lp-cta" onClick={onLaunch}>
            开始交易 →
          </button>
          <button type="button" className="lp-ghost" onClick={onConsole}>
            查看协议数据
          </button>
        </div>

        <div className="lp-ticker">
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">{liveSymbol} 现价</span>
            <span className="lp-ticker-value lp-mono">{priceLabel}</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">强平价格</span>
            <span className="lp-ticker-value lp-accent">无 · None</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">最大亏损</span>
            <span className="lp-ticker-value">本金 + 费用</span>
          </div>
          <div className="lp-ticker-sep" />
          <div className="lp-ticker-item">
            <span className="lp-ticker-label">结算频率</span>
            <span className="lp-ticker-value">每区块</span>
          </div>
        </div>
      </section>

      <section className="lp-section" id="features">
        <div className="lp-section-head">
          <span className="lp-eyebrow">为什么不同</span>
          <h2>把"被强平"这件事从协议里删掉</h2>
          <p>
            传统永续合约靠强平机器人维持偿付能力，代价是用户在最差的时刻被踢出场。
            MoleOption 用完全不同的清算模型，从根本上消除了这种风险。
          </p>
        </div>
        <div className="lp-feature-grid">
          {FEATURES.map((f) => (
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
          <span className="lp-eyebrow">永不爆仓机制</span>
          <h2>传统永续 vs MoleOption</h2>
        </div>
        <div className="lp-compare-grid">
          <div className="lp-compare-col lp-bad">
            <h4>传统永续合约</h4>
            <ul>
              <li>触及维持保证金线即被强平机器人平仓</li>
              <li>极端行情下连环爆仓、插针清算</li>
              <li>清算罚金 + 滑点，亏损可能超过本金</li>
              <li>资金费率持续侵蚀仓位</li>
              <li>归零即出局，无法回本</li>
            </ul>
          </div>
          <div className="lp-compare-col lp-good">
            <h4>MoleOption</h4>
            <ul>
              <li>没有强平引擎，仓位不会被强制平掉</li>
              <li>逐块按真实预言机价格迁移盈亏</li>
              <li>最大亏损锁定为本金 + 开仓费用</li>
              <li>无资金费率，零和透明</li>
              <li>权益归零仍保留仓位，价格反转可复活</li>
            </ul>
          </div>
        </div>
        <p className="lp-disclaimer-inline">
          "永不爆仓"指仓位不会被传统强平，并非"不会亏损"。在极端单边行情中本金仍可能被逐步耗尽。
        </p>
      </section>

      <section className="lp-section" id="how">
        <div className="lp-section-head">
          <span className="lp-eyebrow">运作方式</span>
          <h2>四步走完一笔交易的生命周期</h2>
        </div>
        <div className="lp-steps">
          {STEPS.map((s) => (
            <div key={s.n} className="lp-step">
              <span className="lp-step-n">{s.n}</span>
              <h3>{s.title}</h3>
              <p>{s.body}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="lp-section lp-risk" id="risk">
        <div className="lp-risk-card">
          <h3>诚实的风险提示</h3>
          <ul>
            <li>"永不爆仓"表示仓位不会被传统强平，<b>不表示不会亏完本金</b>。</li>
            <li>最大损失为本金和相关费用。</li>
            <li>显示的盈利为参考值，实际可提取金额取决于市场对手盘的亏损。</li>
            <li>低流动性或严重单边市场可能导致盈利无法全额兑现。</li>
            <li>高杠杆市场本金耗尽速度更快。</li>
            <li>当前权益为 0 不代表仓位被删除；价格反转且出现新对手盘亏损时仓位可重获权益。</li>
          </ul>
          <p className="lp-risk-foot">当前为 Solana Devnet 测试部署，使用测试币，不涉及真实资产。</p>
        </div>
      </section>

      <section className="lp-final">
        <h2>准备好体验没有强平的杠杆交易了吗？</h2>
        <p>连接钱包，挑选一个杠杆分场，几秒内开出你的第一笔不会被爆仓的仓位。</p>
        <button type="button" className="lp-cta lp-cta-lg" onClick={onLaunch}>
          进入交易终端 →
        </button>
      </section>

      <footer className="lp-footer">
        <div className="lp-logo">
          <span className="lp-logo-mark" />
          MoleOption
        </div>
        <span className="lp-footer-note">
          链上 shares 杠杆协议 · Solana · 仅供测试网演示
        </span>
      </footer>
    </div>
  );
}

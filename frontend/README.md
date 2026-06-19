# MoleOption Frontend (wave 11 MVP)

Wave 11 single-page console with three panels:

- **Trader** — open / close positions, watch live mid price
- **Indexer State** — sub-pool stats, dormant inventory, recovery outstanding
- **Keeper Console** — `KeeperLoopMetrics`, vol estimator state, action queue

The data feed is a deterministic mock (`src/mocks/feed.ts`, 800 ms tick) so the UI evolves visibly without any cluster or RPC dependency. Wave 12 will replace the mock with a websocket-backed feed driven by `keeper-rpc`.

## Run

```bash
cd frontend
npm install
npm run dev          # http://localhost:5173
npm run typecheck    # strict TypeScript checks (CI gate)
npm run build        # production build → frontend/dist
```

## Architecture

```
src/
├── App.tsx                  Tab router (Trader / Indexer / Keeper)
├── main.tsx                 React entry
├── styles.css               Single-file CSS (wave 12 → CSS modules)
├── format.ts                Number / pubkey / vol formatters
├── types.ts                 TS mirror of keeper-rpc account fields
├── mocks/feed.ts            useMockFeed() hook
└── panels/
    ├── TraderPanel.tsx      Open / close UI + position table
    ├── IndexerPanel.tsx     Sub-pool / dormant / hint dashboard
    └── KeeperPanel.tsx      Loop metrics + rotation predictions
```

### Wave 12 wiring plan

Replace `useMockFeed` with `useRpcFeed`:

1. WebSocket subscribe to `Market`, `SubPool`, `DormantBucket`, `DistributionLedger` accounts.
2. Decode Borsh payloads via `keeper-rpc::accounts` compiled to wasm (target: `wasm-pack build --target web`).
3. Snapshot reducer combines the streams into the same `FeedSnapshot` shape `mocks/feed.ts` produces today.
4. Submit actions (open / close / claim) via `@solana/web3.js` + Phantom / Backpack / SquadX wallet adapters.

Because the panels consume only the typed `FeedSnapshot`, swapping the feed is a one-file change.

## Config

Vite serves on port `5173` (strict-port). To bind a different port, override in `vite.config.ts`.

## Linting

TypeScript strict mode + `noUnusedLocals` + `noUnusedParameters` + `exactOptionalPropertyTypes`. The build will fail on any unused export, so prune deliberately.

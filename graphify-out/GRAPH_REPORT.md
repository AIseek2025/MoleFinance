# Graph Report - .  (2026-06-19)

## Corpus Check
- 157 files · ~273,020 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 32 nodes · 59 edges · 5 communities detected
- Extraction: 100% EXTRACTED · 0% INFERRED · 0% AMBIGUOUS
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]

## God Nodes (most connected - your core abstractions)
1. `getArrayU8FromWasm0()` - 11 edges
2. `KeeperLeaderLockView` - 8 edges
3. `__wbg_get_imports()` - 6 edges
4. `getUint8ArrayMemory0()` - 5 edges
5. `passStringToWasm0()` - 5 edges
6. `decodeKeeperLeaderLock()` - 4 edges
7. `__wbg_init()` - 4 edges
8. `accountDiscriminator()` - 3 edges
9. `instructionDiscriminator()` - 3 edges
10. `getStringFromWasm0()` - 3 edges

## Surprising Connections (you probably didn't know these)
- `accountDiscriminator()` --calls--> `getArrayU8FromWasm0()`  [EXTRACTED]
  crates/keeper-decoder/pkg/keeper_decoder.js → crates/keeper-decoder/pkg/keeper_decoder.js  _Bridges community 4 → community 0_
- `__wbg_get_imports()` --calls--> `getStringFromWasm0()`  [EXTRACTED]
  crates/keeper-decoder/pkg/keeper_decoder.js → crates/keeper-decoder/pkg/keeper_decoder.js  _Bridges community 3 → community 2_
- `__wbg_get_imports()` --calls--> `passStringToWasm0()`  [EXTRACTED]
  crates/keeper-decoder/pkg/keeper_decoder.js → crates/keeper-decoder/pkg/keeper_decoder.js  _Bridges community 3 → community 4_
- `getArrayU8FromWasm0()` --calls--> `getUint8ArrayMemory0()`  [EXTRACTED]
  crates/keeper-decoder/pkg/keeper_decoder.js → crates/keeper-decoder/pkg/keeper_decoder.js  _Bridges community 0 → community 2_
- `passStringToWasm0()` --calls--> `getUint8ArrayMemory0()`  [EXTRACTED]
  crates/keeper-decoder/pkg/keeper_decoder.js → crates/keeper-decoder/pkg/keeper_decoder.js  _Bridges community 2 → community 4_

## Communities

### Community 0 - "Community 0"
Cohesion: 0.39
Nodes (7): encodeClosePosition(), encodeKeeperLeaderAcquire(), encodeKeeperLeaderHeartbeat(), encodeKeeperLeaderRelease(), encodeOpenPosition(), getArrayU8FromWasm0(), keeperLeaderLockSeedPrefix()

### Community 1 - "Community 1"
Cohesion: 0.33
Nodes (1): KeeperLeaderLockView

### Community 2 - "Community 2"
Cohesion: 0.29
Nodes (6): decodeKeeperLeaderLock(), decodeText(), getStringFromWasm0(), getUint8ArrayMemory0(), passArray8ToWasm0(), takeFromExternrefTable0()

### Community 3 - "Community 3"
Cohesion: 0.4
Nodes (6): getDataViewMemory0(), initSync(), __wbg_finalize_init(), __wbg_get_imports(), __wbg_init(), __wbg_load()

### Community 4 - "Community 4"
Cohesion: 0.67
Nodes (3): accountDiscriminator(), instructionDiscriminator(), passStringToWasm0()

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `KeeperLeaderLockView` connect `Community 1` to `Community 0`, `Community 2`?**
  _High betweenness centrality (0.338) - this node is a cross-community bridge._
- **Why does `getArrayU8FromWasm0()` connect `Community 0` to `Community 1`, `Community 2`, `Community 4`?**
  _High betweenness centrality (0.073) - this node is a cross-community bridge._
- **Why does `decodeKeeperLeaderLock()` connect `Community 2` to `Community 0`?**
  _High betweenness centrality (0.028) - this node is a cross-community bridge._
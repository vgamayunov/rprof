# Offline timeline viewer

The shared viewer supports dark/light themes, including Canvas colors. The toggle
uses the system preference initially and remembers explicit choices when browser
storage is available. Export reports again after rebuilding to include UI updates;
existing standalone HTML files contain their original embedded viewer bundle.

Projection artifacts (`kind: "rprof-projection"`) use `src/projection.ts` and an
offline JSON scenario editor. Their embedded `dist/project.wasm` runs the same Rust
engine as `rprof project`. After changing engine sources or dependencies, install
`wasm32-unknown-unknown` once and run `bash scripts/build-projection-wasm.sh` from
the repository root, then rebuild the viewer. `check-generated` checks both the
viewer bundles and the WASM source/binary manifest. Native/WASM parity is covered
by browser tests; stale engine hashes produce an error while retaining results.

`index.html` is the Rust report template. It contains exactly one each of `{{STYLE}}`, `{{DATA}}`, and `{{SCRIPT}}`; Rust replaces these with the checked-in `dist/viewer.css`, safely escaped JSON, and `dist/viewer.js`. Do not open the template directly to test a capture: it needs an embedded report payload. No browser network access is required.

```sh
cd ui
npm ci
npm run typecheck
npm test
npm run build
npm run check-generated  # compares bundles without writing to dist/
npx playwright install chromium
npm run test:browser
```

`src/model.ts` follows `docs/report-model.md` (version 1). JSON nanoseconds and integer counters remain strings until exact BigInt operations; only view-relative differences are converted to drawing coordinates. The timeline samples extremely dense tracks at most 5,000 spans and 3,000 counter samples per visible row per frame. Zoom in to inspect individual events in dense regions.

Use Node 24 for the TypeScript-strip unit test runner. Browser tests build the Rust
CLI, generate reports from the checked-in fixture, and exercise the actual offline
artifact in Chromium. Unit tests cover geometry, interval culling, exact integer
counter deltas, filtering, and separating counter series by unit and value type.

Stable browser-test hooks: `#domain-select` (one unaligned domain at a time), `#filter-input`, `#zoom-in`, `#zoom-out`, `#fit-view` (Reset), `#timeline-scroll`, `#timeline-canvas`, `#time-ruler`, `#range-label`, `#detail-content`, `#category-table`, `#capture-status`, `#header-meta`. Click a track or shard label to collapse/expand; click bars/points or filled utilization areas for exact values and metadata. Wheel/trackpad scrolling moves vertically over labels and plots. Zoom with buttons or `+`/`−`; Reset/Home fits the full time range. Drag pans horizontally. Long labels are ellipsized; hover for their full text.

# NetWeevil web console

This folder contains the source of the NetWeevil web console. The console is a
React map application for the NetWeevil HTTP API. The `netweevil` executable
embeds the built console and serves it at `/`.

The user guide is on the documentation site:
[Web console](https://elqniemi.github.io/netweevil/tools/web-console/)
([source](../docs/tools/web-console.md)).

The console uses Vite, React 19, TanStack Query, MapLibre GL and deck.gl.

## Requirements

- Node.js 22
- pnpm

## Build

1. Install the dependencies:

   ```bash
   pnpm install
   ```

2. Build the static bundle. This writes `frontend/dist/`.

   ```bash
   pnpm build
   ```

3. Build the executable from the repository root. The build embeds
   `frontend/dist` when it exists.

   ```bash
   cargo build --release -p netweevil-cli
   ```

## Develop

1. Start an API on `127.0.0.1:8080`, for example with
   `netweevil bootstrap --no-open`.
2. Start the development server:

   ```bash
   pnpm dev
   ```

3. Open `http://localhost:5173`. The development server sends `/v1` requests
   to `127.0.0.1:8080`.

Set `NETWEEVIL_API=http://host:port` to use a different API.

In development, `window.__netweevil` gives access to the store and the run
helpers from the browser console.

## Test

Run these checks before you commit a change:

```bash
pnpm typecheck
pnpm test
```

The tests are in `tests/*.test.mjs`. They use the Node.js test runner.

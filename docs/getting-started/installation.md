# Installation

You can install NetWeevil in three ways:

- Build it from source with the Rust toolchain.
- Use a prebuilt release archive.
- Build and run a Docker image. See [Docker](docker.md).

## Requirements

| Item | Version | Use |
| ---- | ------- | --- |
| Rust toolchain | `1.96.1` or later, edition 2024 | Build the `netweevil` binary. |
| Node.js and pnpm | Node.js 22 | Optional. Build the web console. |
| Docker and Docker Compose | Current | Optional. Run the API in a container. |
| QGIS | `3.28` to `4.99` | Optional. Use the QGIS plugin. |
| `curl` | Any | Send requests to the API. |
| `osmium` | Any | Optional. Filter OSM extracts. |

## Build from source

1.  Install the Rust toolchain from [rustup.rs](https://rustup.rs).
2.  Install the toolchain version that the workspace declares:

    ```bash
    rustup toolchain install 1.96.1
    ```

3.  Get the source code:

    ```bash
    git clone https://github.com/elqniemi/netweevil.git
    cd netweevil
    ```

4.  Build the release binary:

    ```bash
    cargo build --release -p netweevil-cli
    ```

5.  Make sure that the binary runs:

    ```bash
    target/release/netweevil --help
    ```

The binary is `target/release/netweevil`. Copy it to a directory in your
`PATH`, or run it with its full path.

!!! note
    The pages of this site use `netweevil` as the command name. In a source
    checkout, you can use `cargo run --release -p netweevil-cli --` instead.

## Build the web console (optional)

The API binary can contain the web console. The build script of the API
crate embeds `frontend/dist` when that directory exists at compile time.
Without it, the binary serves the API only.

1.  Install Node.js 22 from [nodejs.org](https://nodejs.org).
2.  Enable pnpm:

    ```bash
    corepack enable
    ```

3.  Build the console:

    ```bash
    cd frontend
    pnpm install --frozen-lockfile
    pnpm build
    cd ..
    ```

4.  Build the binary again. The build embeds the new `frontend/dist`:

    ```bash
    cargo build --release -p netweevil-cli
    ```

!!! tip
    You can also serve a console directory that is not embedded. Use
    `netweevil api serve --console-dir frontend/dist`.

## Start script

The script `scripts/start.sh` (macOS and Linux) builds the missing parts and
starts NetWeevil. On Windows, use `scripts\start.cmd`.

1.  Run the script from the repository:

    ```bash
    ./scripts/start.sh
    ```

2.  The script builds the web console if `frontend/dist/index.html` does not
    exist. It uses pnpm, or npm if pnpm is not installed.
3.  The script builds `target/release/netweevil` if the binary does not
    exist.
4.  The script starts `netweevil bootstrap` in the workspace `~/NetWeevil`.
    The browser opens the console.

To use a different workspace, set `NETWEEVIL_WORKSPACE`:

```bash
NETWEEVIL_WORKSPACE=/path/to/workspace ./scripts/start.sh
```

To use a different address, set `NETWEEVIL_BIND`. The default is
`127.0.0.1:8080`.

## Prebuilt archive

A release archive contains the binary and a start script.

1.  Extract the archive.
2.  Run `start-netweevil.sh`.

The archive keeps its data in `~/NetWeevil`. Maintainers make the archive
with `scripts/package_release.sh`.

## Run the checks

Contributors run these checks before they send a change:

```bash
cargo +1.96.1 fmt --all
cargo +1.96.1 check
cargo +1.96.1 test
```

## Next steps

- Calculate a first route in the [Quickstart](quickstart.md).
- Install the [QGIS plugin](../tools/qgis-plugin.md).

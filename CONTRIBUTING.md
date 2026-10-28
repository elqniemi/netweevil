# Contributing to NetWeevil

This document tells you how to set up a development environment, which checks
to run, and how to send a change.

## Set up the development environment

1. Install Rust with rustup. The workspace uses edition 2024 and
   `rust-version = 1.96.1`.
2. Clone the repository.
3. Build the workspace and run the tests:

    ```bash
    cargo check
    cargo test
    ```

    The tests use fixtures in the repository. They do not need datasets.

4. Optional: install Node.js 22 and pnpm to work on the web console in
   `frontend/`.
5. Optional: install Python 3 to run the scripts and the script tests.
6. Optional: install [uv](https://docs.astral.sh/uv/) to build the
   documentation site.
7. Optional: download the sample datasets for end-to-end work. Follow
   [`datasets/README.md`](https://github.com/elqniemi/netweevil/blob/main/datasets/README.md).

The [Quickstart](https://github.com/elqniemi/netweevil/blob/main/README.md#quickstart) imports a dataset and starts the API.

## Run the checks

Run these checks before you open a pull request.

### Rust

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Clippy must give no warnings.

### Web console

Run these checks when you change `frontend/`:

```bash
pnpm --dir frontend install
pnpm --dir frontend typecheck
pnpm --dir frontend test
```

### Python scripts

Run the script tests when you change `scripts/`:

```bash
python3 -m unittest discover -s scripts -p 'test_*.py'
```

### QGIS plugin

Do these checks when you change `qgis_plugin/`:

1. Compile the modules with
   `python3 -m py_compile qgis_plugin/netweevil_qgis/*.py`.
2. Make the package with `scripts/package_qgis_plugin.sh`.
3. If possible, load the package in QGIS 3 and QGIS 4.

### Documentation

Build the documentation site in strict mode when you change `docs/`,
`mkdocs.yml` or a file that the site includes:

```bash
uvx --with-requirements docs/requirements.txt mkdocs build --strict
```

A strict build fails on broken links and missing pages.

### OpenAPI document

The OpenAPI document `docs/api/openapi.yaml` must match the API router. The
tests in `crates/api/src/api_docs/tests.rs` compare the two. They fail when a
route has no operation in the document, or when the document has an operation
that the router does not serve.

When you add, remove or change an API route, update `docs/api/openapi.yaml` in
the same change. Then run:

```bash
cargo test -p netweevil-api api_docs
```

## Rules for changes

- **Keep results exact.** Do not add budgets or heuristics that can give a
  path that is not optimal. The accelerated engine (CCH) must give the same
  costs as the exact engine. Add an equivalence test against the exact engine
  for each new fast path.
- Use the existing crate boundaries and request and result models. Do not add
  shortcuts between crates. [AGENTS.md](https://github.com/elqniemi/netweevil/blob/main/AGENTS.md) lists what each crate owns.
- Keep persisted state in `.netweevil/`. Do not commit local state, imported
  datasets or large derived bundles.
- When you change CLI or API behavior, update the documentation pages and the
  `examples/` files for the commands and requests that change.
- The QGIS plugin supports QGIS 3.28 to 4.99. Keep Qt 5 and Qt 6
  compatibility. Put version differences in
  `qgis_plugin/netweevil_qgis/compat.py`.
- Add focused tests in the crate that owns the behavior. For routing, profile
  and query changes, use the `examples/` fixtures when possible.
- Write documentation in ASD-STE100 Simplified Technical English: short
  sentences, active voice, and one instruction in each step.

## Write commit messages

- Write the subject line in the imperative mood, for example
  `Serve a dedicated readiness probe on /readyz`.
- Start the subject with a capital letter. Do not end it with a period.
- Do not add a type prefix such as `feat:` or `fix:`.
- Keep the subject at 72 characters or less.
- Use the body to tell why the change is necessary, when the subject does not
  make it clear.
- Make one commit for each logical change.

## Send a pull request

1. Make a branch from `main`.
2. Keep the pull request on one topic.
3. Describe what changed and why.
4. For a performance claim, give the dataset, the hardware, and the values
   before and after the change.
5. Run the checks in this guide before you open the pull request.

Maintainers squash-merge pull requests, unless the branch history has a
deliberate structure.

## Report a problem

Use the issue templates. For a bug, give a minimal reproduction:

- the dataset or a public extract,
- the profile,
- the request,
- the result that you saw and the result that you expected.

Report security problems as [SECURITY.md](https://github.com/elqniemi/netweevil/blob/main/SECURITY.md) describes.

## License of contributions

Your contributions are available under the MIT license and the Apache License
2.0, the same as the repository. See [LICENSE](https://github.com/elqniemi/netweevil/blob/main/LICENSE).

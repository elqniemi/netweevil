# Security policy

## Supported versions

NetWeevil has no 1.0 release. Only the latest `main` branch gets security
fixes.

## Report a vulnerability

Do not open a public issue for a security vulnerability.

1. Open the **Security** tab of the
   [repository](https://github.com/elqniemi/netweevil).
2. Click **Report a vulnerability**. GitHub sends the report to the
   maintainers only.
3. Give this information, if you have it:
    - the affected component: CLI, HTTP API, web console, QGIS plugin or
      ingest,
    - the steps to reproduce the problem,
    - the effect that you expect the problem to have.

The maintainers reply within one week. The maintainers name the reporter in
the release notes, unless the reporter asks them not to.

## Scope

The HTTP API is for local use or use on a trusted network. It has no
authentication and it sends permissive CORS headers. Reports about the API on
an untrusted network are also welcome, because some deployments use it there.

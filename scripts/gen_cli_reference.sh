#!/usr/bin/env bash
# Write docs/cli/reference.md from the `--help` output of every
# `netweevil` command and subcommand.
#
# Usage:
#   scripts/gen_cli_reference.sh                 # builds target/debug/netweevil
#   NETWEEVIL_BIN=/path/to/netweevil scripts/gen_cli_reference.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${repo_root}/docs/cli/reference.md"

if [[ -z "${NETWEEVIL_BIN:-}" ]]; then
  cargo build --quiet --manifest-path "${repo_root}/Cargo.toml" -p netweevil-cli
  NETWEEVIL_BIN="${repo_root}/target/debug/netweevil"
fi

# Print the subcommand names listed under "Commands:" in a help text.
subcommands() {
  awk '
    /^Commands:/ { in_commands = 1; next }
    in_commands && /^[^ ]/ { in_commands = 0 }
    in_commands && /^  [a-z]/ { print $1 }
  ' | grep -vx help || true
}

# Write the section of one command, then recurse into its subcommands.
# Arguments: heading level, then the command path (empty for the root).
emit() {
  local level="$1"
  shift
  local help
  help="$("${NETWEEVIL_BIN}" "$@" --help)"
  local hashes
  hashes="$(printf '#%.0s' $(seq 1 "${level}"))"
  local title="netweevil${*:+ $*}"
  local anchor="${title// /-}"
  printf '%s `%s` { #%s }\n\n```text\n%s\n```\n\n' \
    "${hashes}" "${title}" "${anchor}" "${help}"
  local sub
  for sub in $(printf '%s\n' "${help}" | subcommands); do
    emit $((level + 1)) "$@" "${sub}"
  done
}

mkdir -p "$(dirname "${out}")"
{
  cat <<'EOF'
# CLI reference

This page contains the `--help` output of each `netweevil` command.

!!! note "Generated page"
    Do not edit this page. The script `scripts/gen_cli_reference.sh` writes
    it from the built binary. To regenerate the page, run the script from the
    repository root:

    ```bash
    scripts/gen_cli_reference.sh
    ```

EOF
  emit 2
} >"${out}"

echo "wrote ${out}"

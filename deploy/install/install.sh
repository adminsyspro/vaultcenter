#!/usr/bin/env bash
# shellcheck disable=SC2317
# (SC2317, unreachable code, for the whole file: the steps are called by name,
# as "step_${name}", and the exit and signal handlers from traps; shellcheck
# follows neither and would report every one of them.)
#
# VaultCenter host installer.
#
#   curl -fsSL https://vaultcenter.io/install | sudo bash
#
# Exit codes: 0 success, 2 usage, 3 refusal, 4 step failure,
# 5 update cancelled and rolled back, 6 rollback failed; 130, 143, 129 on
# SIGINT, SIGTERM, SIGHUP, except while the updater runs (see on_signal).
# On an installed appliance the script is an updater front end: it pulls the
# target image and hands over to the update.py shipped in that image.
set -euo pipefail

# Every path read or written lives under ${ROOT} (empty on a real machine).
ROOT="${VAULTCENTER_INSTALL_ROOT:-}"
LOG_FILE="${ROOT}/var/log/vaultcenter-install.log"
STATE_FILE="${ROOT}/var/lib/vaultcenter-install/state"
HOST_DIR="${ROOT}/etc/vaultcenter-host"
DOCKER_CLIENT_DIR="${ROOT}/etc/vaultcenter-install/docker"
PAYLOAD_ROOT="${ROOT}/usr/local/share/vaultcenter"
IMAGE_REPOSITORY="ghcr.io/adminsyspro/vaultcenter"
VALIDATE_URL="https://vaultcenter.io/api/v1/install/validate"
RESUME_HINT="run the same installation command again, it resumes at this step"
LEGACY_HOST_DIR="${ROOT}/etc/freezer-host"
APPLIANCE_CONTAINER="vaultcenter-appliance-1"
LEGACY_CONTAINER="freezer-appliance-1"
# X.Y.Z, or X.Y.Z-<pre-release> with semver pre-release identifiers.
VERSION_REGEX='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
# What a resumed installation says when the host installer stopped after
# writing compose.json. docs/install.md quotes both sentences.
HOST_LEFTOVER_CAUSE="the installation of the host storage service stopped part-way during an earlier run: /etc/vaultcenter-host/compose.json exists although the step is not finished, and the host installer refuses to run over an existing configuration. Nothing has been deleted. Concerned: the /etc/vaultcenter-host directory and the Docker volumes vaultcenter_state and vaultcenter_media. Deleting them is your decision: do so only if this appliance has never started and they hold nothing worth keeping."
HOST_LEFTOVER_RESUME="after deleting them yourself (rm -r /etc/vaultcenter-host ; docker volume rm vaultcenter_state vaultcenter_media), run the same installation command again"

TOKEN=""
REGISTRY_TOKEN=""
REGISTRY_USER=""
VERSION=""
ASSUME_YES=0
QUIET=0
SKIP_PREFLIGHT=0
MODE=""
RESUME_ROLLBACK=0
FORCE_ROLLBACK=0

# Obtained by the credentials step, kept in memory only (never exported, never
# in the state file, never on a command line).
REGISTRY=""
REGISTRY_PASSWORD=""
# The administrator password lives here from its creation until the summary
# prints it: never exported, never written, redacted from the log.
ADMIN_PASSWORD=""
IMAGE_REF=""
IMAGE_VERSION=""
# What cleanup undoes at exit, on failure and on SIGINT/SIGTERM. Later tasks
# extend cleanup() rather than setting their own trap.
PAYLOAD_CONTAINER=""
LOGIN_REGISTRY=""

# ---------------------------------------------------------------- steps
# Later tasks add a function step_<name> and, when the step is new, one
# register_step line. main runs them in registration order.
STEP_NAMES=()
STEP_LABELS=()
register_step() {
    STEP_NAMES+=("$1")
    STEP_LABELS+=("$2")
}

register_step preflight "Preflight checks"
register_step credentials "Registry credentials"
register_step docker "Docker"
register_step image "VaultCenter image"
register_step payload "Host files"
register_step update "Appliance update"
register_step kernel_module "mhvtl kernel module"
register_step host_service "Host storage service"
register_step configuration "Configuration"
register_step administrator "Administrator account"
register_step start "Startup"
register_step summary "Summary"

# Update mode runs only these steps; the other modes never run "update".
UPDATE_STEPS=" preflight credentials docker image payload update summary "

# select_steps: keeps the steps of the detected mode, in registration order.
select_steps() {
    local names=() labels=() index name
    for index in "${!STEP_NAMES[@]}"; do
        name="${STEP_NAMES[${index}]}"
        if [[ "${MODE}" == "update" ]]; then
            [[ "${UPDATE_STEPS}" == *" ${name} "* ]] || continue
        elif [[ "${name}" == "update" ]]; then
            continue
        fi
        names+=("${name}")
        labels+=("${STEP_LABELS[${index}]}")
    done
    STEP_NAMES=("${names[@]}")
    STEP_LABELS=("${labels[@]}")
}

# ------------------------------------------------------------ rendering
FANCY=0
BOLD="" BRAND="" GREEN="" RED="" RESET=""
CURRENT_STEP=0
CURRENT_LABEL=""
STEP_OPEN=0

setup_rendering() {
    if [[ -t 1 && "${QUIET}" -eq 0 ]]; then
        FANCY=1
    fi
    if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
        BOLD=$'\e[1m' BRAND=$'\e[1;36m' GREEN=$'\e[32m' RED=$'\e[31m' RESET=$'\e[0m'
    fi
}

banner() {
    if [[ "${FANCY}" -eq 1 ]]; then
        if [[ "${MODE}" == "update" ]]; then
            printf '%s\n  VaultCenter%s - update\n\n' "${BRAND}" "${RESET}"
        else
            printf '%s\n  VaultCenter%s - installation\n\n' "${BRAND}" "${RESET}"
        fi
    fi
}

# step <n> <label>: announces a step.
step() {
    CURRENT_STEP="$1"
    CURRENT_LABEL="$2"
    STEP_OPEN=1
    log "step ${1}/${#STEP_NAMES[@]}: ${2}"
    if [[ "${FANCY}" -eq 1 ]]; then
        printf '%s[%s/%s]%s %s\n' "${BOLD}" "$1" "${#STEP_NAMES[@]}" "${RESET}" "$2"
    fi
}

# ok [result]: the current step succeeded.
ok() {
    local result="${1:-ok}"
    STEP_OPEN=0
    log "ok: ${result}"
    if [[ "${FANCY}" -eq 1 ]]; then
        printf '      %s✔%s %s\n' "${GREEN}" "${RESET}" "${result}"
    else
        printf '[%s/%s] %s: %s\n' "${CURRENT_STEP}" "${#STEP_NAMES[@]}" "${CURRENT_LABEL}" "${result}"
    fi
}

# not_ok <result>: the current step ended, and not well; the caller goes on (the
# summary tells the rest and sets the exit code).
not_ok() {
    STEP_OPEN=0
    log "unsuccessful: $1"
    if [[ "${FANCY}" -eq 1 ]]; then
        printf '      %s✘%s %s\n' "${RED}" "${RESET}" "$1"
    else
        printf '[%s/%s] %s: %s\n' "${CURRENT_STEP}" "${#STEP_NAMES[@]}" "${CURRENT_LABEL}" "$1"
    fi
}

# fail <cause> <resume command>: the current step failed, exit 4.
fail() {
    local cause="$1" resume="${2:-}"
    log "FAIL step ${CURRENT_STEP} (${CURRENT_LABEL}): ${cause}"
    if [[ "${STEP_OPEN}" -eq 1 ]]; then
        if [[ "${FANCY}" -eq 1 ]]; then
            printf '      %s✘%s failed\n' "${RED}" "${RESET}"
        else
            printf '[%s/%s] %s: failed\n' "${CURRENT_STEP}" "${#STEP_NAMES[@]}" "${CURRENT_LABEL}"
        fi
    fi
    {
        printf '%sFailed%s at step %s (%s): %s\n' "${RED}" "${RESET}" "${CURRENT_STEP}" "${CURRENT_LABEL}" "${cause}"
        if [[ -n "${resume}" ]]; then
            printf 'To resume: %s\n' "${resume}"
        fi
        printf 'Log: %s\n' "/var/log/vaultcenter-install.log"
    } >&2
    exit 4
}

# refuse <message>: the machine is not in a state the installer may touch, exit 3.
refuse() {
    log "REFUSED: $1"
    printf '%sRefused%s: %s\n' "${RED}" "${RESET}" "$1" >&2
    exit 3
}

# warn <message>: something the operator must know that does not stop the run.
warn() {
    log "WARNING: $1"
    printf 'Warning: %s\n' "$1" >&2
}

usage_error() {
    printf 'Error: %s\n' "$1" >&2
    cat >&2 <<'USAGE'
Usage: install.sh [--token <token> | --registry-token <token> --registry-user <name>]
                  [--version X.Y.Z] [--yes] [--quiet] [--skip-preflight]
                  [--resume-rollback [--force-rollback]]
--version also accepts a pre-release (X.Y.Z-rc.1).
On an appliance that is already installed, the script updates it (--version
chooses the target version). --resume-rollback restores the backup of an
interrupted update: pass the same --version as the interrupted update, the
script needs the image and the files of that version. --force-rollback (with
--resume-rollback only) restores even if the appliance runs in good health in
the target version.
USAGE
    exit 2
}

# ------------------------------------------------------------------ log
# Secrets are replaced by *** in everything that reaches the log: the tokens,
# the passwords, and the password a proxy URL may carry.
PROXY_SECRETS=()
redact() {
    local text="$1" secret
    for secret in "${TOKEN}" "${REGISTRY_TOKEN}" "${REGISTRY_PASSWORD}" "${ADMIN_PASSWORD}" "${PROXY_SECRETS[@]}"; do
        if [[ -n "${secret}" ]]; then
            text="${text//"${secret}"/***}"
        fi
    done
    printf '%s' "${text}"
}

init_log() {
    mkdir -p "$(dirname "${LOG_FILE}")"
    (umask 077 && : >>"${LOG_FILE}")
    chmod 600 "${LOG_FILE}"
}

log() {
    if [[ -w "${LOG_FILE}" ]]; then
        printf '%s %s\n' "$(date -u +%FT%TZ)" "$(redact "$1")" >>"${LOG_FILE}"
    fi
}

# run_logged <cmd...>: runs the command, logs it and its output (never shown
# unless the caller prints RUN_OUTPUT), returns its status.
RUN_OUTPUT=""
run_logged() {
    local rc=0
    log "\$ $*"
    RUN_OUTPUT="$("$@" 2>&1)" || rc=$?
    if [[ -n "${RUN_OUTPUT}" ]]; then
        log "${RUN_OUTPUT}"
    fi
    log "exit ${rc}"
    return "${rc}"
}

# run_shown <cmd...>: run_logged for a long command: its output is also shown
# as it comes (on standard error), so that minutes of work do not look like a
# hang. Nothing secret may be in that output.
run_shown() {
    local rc=0
    log "\$ $*"
    { RUN_OUTPUT="$("$@" 2>&1 | tee /dev/fd/3)" || rc=$?; } 3>&2
    if [[ -n "${RUN_OUTPUT}" ]]; then
        log "${RUN_OUTPUT}"
    fi
    log "exit ${rc}"
    return "${rc}"
}

# ----------------------------------------------------------- terminal
has_tty() {
    { : </dev/tty; } 2>/dev/null
}

# ask_yes <question>: asks on the terminal, never on stdin (the script itself
# may be on stdin when run as curl | bash).
ask_yes() {
    local answer=""
    printf '%s [y/N] ' "$1" >/dev/tty
    read -r answer </dev/tty || true
    case "${answer}" in
        o | O | oui | OUI | y | Y | yes) return 0 ;;
        *) return 1 ;;
    esac
}

# -------------------------------------------------------------- options
parse_options() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --token | --registry-token | --registry-user | --version)
                [[ $# -ge 2 && "$2" != --* ]] || usage_error "option $1 expects a value"
                case "$1" in
                    --token) TOKEN="$2" ;;
                    --registry-token) REGISTRY_TOKEN="$2" ;;
                    --registry-user) REGISTRY_USER="$2" ;;
                    --version) VERSION="$2" ;;
                esac
                shift 2
                ;;
            --yes) ASSUME_YES=1; shift ;;
            --quiet) QUIET=1; shift ;;
            --skip-preflight) SKIP_PREFLIGHT=1; shift ;;
            --resume-rollback) RESUME_ROLLBACK=1; shift ;;
            --force-rollback) FORCE_ROLLBACK=1; shift ;;
            # Only the name: --token=<secret> must not be echoed.
            *) usage_error "unknown option: ${1%%=*}" ;;
        esac
    done
    if [[ -n "${TOKEN}" && -n "${REGISTRY_TOKEN}" ]]; then
        usage_error "--token and --registry-token cannot be used together"
    fi
    if [[ -n "${REGISTRY_TOKEN}" && -z "${REGISTRY_USER}" ]]; then
        usage_error "--registry-token requires --registry-user <name>"
    fi
    if [[ -n "${REGISTRY_USER}" && -z "${REGISTRY_TOKEN}" ]]; then
        usage_error "--registry-user is only used with --registry-token <token>"
    fi
    if [[ "${FORCE_ROLLBACK}" -eq 1 && "${RESUME_ROLLBACK}" -eq 0 ]]; then
        usage_error "--force-rollback is only used with --resume-rollback"
    fi
    if [[ -n "${VERSION}" && ! "${VERSION}" =~ ${VERSION_REGEX} ]]; then
        usage_error "--version expects X.Y.Z or X.Y.Z-<pre-release>, got: ${VERSION}"
    fi
}

require_root() {
    if [[ "$(id -u)" != "0" ]]; then
        refuse "this script must be run as root (sudo bash ...)."
    fi
}

# -------------------------------------------------------- mode choice
# container_exists <name>: asked to docker only when docker is installed.
container_exists() {
    command -v docker >/dev/null 2>&1 || return 1
    docker ps -a --format '{{.Names}}' 2>/dev/null | grep -Fxq "$1"
}

# detect_mode: sets MODE to install|update|resume|legacy|mixed and FOUND to
# what it saw on the machine (set globally: not called in a subshell).
FOUND=""
detect_mode() {
    local has_host=0 has_compose=0 has_state=0 has_container=0
    FOUND=""
    if [[ -d "${LEGACY_HOST_DIR}" ]]; then
        FOUND+=" ${LEGACY_HOST_DIR#"${ROOT}"}"
    fi
    if container_exists "${LEGACY_CONTAINER}"; then
        FOUND+=" container ${LEGACY_CONTAINER}"
    fi
    if [[ -n "${FOUND}" ]]; then
        MODE=legacy
        return
    fi
    if [[ -d "${HOST_DIR}" ]]; then has_host=1; FOUND+=" ${HOST_DIR#"${ROOT}"}"; fi
    if [[ -f "${HOST_DIR}/compose.json" ]]; then has_compose=1; FOUND+=" ${HOST_DIR#"${ROOT}"}/compose.json"; fi
    if [[ -f "${STATE_FILE}" ]]; then has_state=1; FOUND+=" ${STATE_FILE#"${ROOT}"}"; fi
    if container_exists "${APPLIANCE_CONTAINER}"; then has_container=1; FOUND+=" container ${APPLIANCE_CONTAINER}"; fi
    if [[ "${has_compose}" -eq 1 ]]; then
        # The host service writes compose.json mid-installation: only the
        # installing marker of an unfinished install makes this a resumed run.
        if [[ "${has_state}" -eq 1 ]] && grep -qxF 'installing=1' "${STATE_FILE}"; then
            MODE=resume
        else
            MODE=update
        fi
    elif [[ "${has_state}" -eq 1 ]]; then
        MODE=resume
    elif [[ "${has_host}" -eq 0 && "${has_container}" -eq 0 ]]; then
        MODE=install
    else
        MODE=mixed
    fi
}

choose_mode() {
    detect_mode
    log "mode=${MODE}"
    if [[ "${MODE}" != "update" && "${RESUME_ROLLBACK}" -eq 1 ]]; then
        usage_error "--resume-rollback and --force-rollback are only used on an appliance that is already installed (update mode); detected mode: ${MODE}"
    fi
    case "${MODE}" in
        legacy)
            refuse "an installation under the former product name was found (${FOUND# }). Nothing has been changed: this installer does not convert it."
            ;;
        mixed)
            refuse "the state of the machine is inconsistent, nothing has been changed. Found: ${FOUND# }."
            ;;
    esac
}

confirm() {
    if [[ "${ASSUME_YES}" -eq 1 ]]; then
        return
    fi
    if ! has_tty; then
        usage_error "no terminal available to confirm: add --yes"
    fi
    local question="Install VaultCenter on this machine?"
    if [[ "${MODE}" == "resume" ]]; then
        question="Resume the interrupted installation of VaultCenter on this machine?"
    elif [[ "${MODE}" == "update" && "${RESUME_ROLLBACK}" -eq 1 ]]; then
        question="Cancel the interrupted update of this VaultCenter appliance (rollback)?"
    elif [[ "${MODE}" == "update" ]]; then
        question="Update the VaultCenter appliance on this machine?"
    fi
    ask_yes "${question}" || refuse "cancelled by the operator, nothing has been changed."
}

# resume_version_conflict <recorded version>: a resumed installation keeps the
# image it began with; asking for another version is refused (exit 2) rather
# than silently ignored.
resume_version_conflict() {
    local recorded="$1" state="${STATE_FILE#"${ROOT}"}"
    log "REFUSED: --version ${VERSION} while the interrupted installation is at version ${recorded}"
    {
        printf 'Error: an installation of version %s began on this machine and did not finish, and --version asks for %s. Nothing has been changed.\n' "${recorded}" "${VERSION}"
        printf 'To finish it: run the same command again without --version, or with --version %s.\n' "${recorded}"
        printf 'To start over with version %s: delete %s, then run again. If %s already exists, first follow "Resuming after an interruption" in %s.\n' \
            "${VERSION}" "${state}" "${HOST_DIR#"${ROOT}"}" "$(doc_path)"
    } >&2
    exit 2
}

# doc_path: where the operator finds the installation guide: the copy shipped
# in the image once its host files are extracted, else its place in the repository.
doc_path() {
    local version="${IMAGE_VERSION}" file=""
    if [[ -z "${version}" ]]; then
        version="$(state_value version)"
    fi
    file="${PAYLOAD_ROOT}/${version}/host/install.md"
    if [[ -n "${version}" && -f "${file}" ]]; then
        printf '%s' "${file#"${ROOT}"}"
    else
        printf 'docs/install.md'
    fi
}

# ------------------------------------------------------------ preflight
# The checks collect every cause before refusing, so the operator fixes the
# machine once. They only read, with one exception: the apt lists are refreshed
# (apt-get update) before apt-cache is asked about a package. The checks
# install and write nothing else; step_preflight installs the tools that are
# missing once the safety checks have passed.
PROBLEMS=()
problem() {
    PROBLEMS+=("$1")
}

# preflight_conclude: refuses (exit 3) with every cause found, if any.
PREFLIGHT_INSTALLED=""
preflight_conclude() {
    if [[ "${#PROBLEMS[@]}" -eq 0 ]]; then
        return 0
    fi
    local message cause
    message="the machine is not suitable, nothing has been changed (${#PROBLEMS[@]} cause(s)):"
    if [[ -n "${PREFLIGHT_INSTALLED}" ]]; then
        message="the machine is not suitable; only the packages${PREFLIGHT_INSTALLED} have been installed, nothing else has been changed (${#PROBLEMS[@]} cause(s)):"
    fi
    for cause in "${PROBLEMS[@]}"; do
        message+=$'\n'"  - ${cause}"
    done
    refuse "${message}"
}

os_release_value() {
    local line=""
    line="$(grep -m1 "^$1=" "${ROOT}/etc/os-release" 2>/dev/null || true)"
    line="${line#*=}"
    printf '%s' "${line//\"/}"
}

port_in_use() {
    ss -H -ltn 2>/dev/null | awk -v port=":$1" '{ n = length($4); if (substr($4, n - length(port) + 1) == port) found = 1 } END { exit !found }'
}

host_daemon_found() {
    local unit process
    for unit in tgt.service mhvtl.target; do
        if [[ "$(systemctl is-active "${unit}" 2>/dev/null || true)" == "active" ]]; then
            return 0
        fi
    done
    for process in tgtd vtltape vtllibrary; do
        if pgrep -x "${process}" >/dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

# kernel_headers_metapackage <kernel>: the Debian package that follows the
# flavour of the kernel, so that every kernel upgrade brings its headers and
# DKMS can rebuild the module for it; empty for a flavour not known here.
kernel_headers_metapackage() {
    case "$1" in
        *-cloud-amd64) printf 'linux-headers-cloud-amd64' ;;
        *-rt-amd64) printf 'linux-headers-rt-amd64' ;;
        *-amd64) printf 'linux-headers-amd64' ;;
    esac
}

# apt_has_candidate <package>: apt can install the package (or already has).
apt_has_candidate() {
    local candidate=""
    candidate="$(apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ { print $2 }')" || candidate=""
    [[ -n "${candidate}" && "${candidate}" != "(none)" ]]
}

# preflight_apt_lists: apt-cache answers from the apt lists, which are empty on
# a fresh cloud image: refresh them first, once per run (apt_install does not
# refresh them again). Returns non-zero, with the cause recorded, when the
# refresh failed: apt-cache then proves nothing.
APT_UPDATED=0
APT_REFRESH_FAILED=0
preflight_apt_lists() {
    if [[ "${APT_UPDATED}" -eq 1 ]]; then
        return 0
    fi
    if [[ "${APT_REFRESH_FAILED}" -eq 1 ]]; then
        return 1
    fi
    if apt_call update; then
        APT_UPDATED=1
        return 0
    fi
    APT_REFRESH_FAILED=1
    problem "apt-get update failed (${RUN_OUTPUT##*$'\n'}): the apt repositories must be reachable to install the required packages (kernel headers, tools). Fix the apt sources or the network, then run again."
    return 1
}

# preflight_kernel_headers <kernel>: the headers of the running kernel must be
# installed or installable; before the kernel module step, so must the package
# that will follow the kernel upgrades.
preflight_kernel_headers() {
    local kernel="$1" meta=""
    if [[ ! -d "${ROOT}/lib/modules/${kernel}/build" ]]; then
        preflight_apt_lists || return 0
        if ! apt_has_candidate "linux-headers-${kernel}"; then
            problem "the headers of the running kernel (${kernel}) cannot be found (package linux-headers-${kernel}): run apt-get update; if they still cannot be found, this kernel is no longer in the repositories: update the kernel (apt-get dist-upgrade), reboot the machine, then run again."
        fi
    fi
    if [[ "${MODE}" == "update" ]] || step_is_done kernel_module; then
        return 0
    fi
    meta="$(kernel_headers_metapackage "${kernel}")"
    if [[ -n "${meta}" ]]; then
        preflight_apt_lists || return 0
        if ! apt_has_candidate "${meta}"; then
            problem "the package ${meta} cannot be found in the apt repositories: it brings the headers along with every kernel update, without which the mhvtl module would be missing after a reboot. Run apt-get update and check the apt sources, then run again."
        fi
    fi
}

# preflight_safety <install|update> [defer]: what the installer must never
# touch. Update mode skips what the running appliance legitimately occupies
# (ports, host daemons). With "defer" the refusal is left to preflight_conclude.
preflight_safety() {
    local mode="$1" defer="${2:-}" version kernel virt
    version="$(os_release_value VERSION_ID)"
    if [[ "$(os_release_value ID)" != "debian" || ! "${version}" =~ ^(12|13)$ ]]; then
        problem "unsupported system: Debian 12 or 13 required (found: $(os_release_value ID) ${version:-unknown})."
    fi
    if [[ "$(uname -m)" != "x86_64" ]]; then
        problem "unsupported architecture $(uname -m): amd64 (x86_64) required."
    fi
    if [[ ! -d "${ROOT}/run/systemd/system" ]]; then
        problem "systemd is not the init system of this machine."
    fi
    if [[ ! -e "${ROOT}/sys/fs/cgroup/cgroup.controllers" ]]; then
        problem "cgroup v2 required (unified hierarchy missing)."
    fi
    virt="$(systemd-detect-virt --container 2>/dev/null || true)"
    if [[ -n "${virt}" && "${virt}" != "none" ]]; then
        problem "the machine is a container (${virt}): a virtual or physical machine is required."
    fi
    if command -v docker >/dev/null 2>&1 \
        && docker info --format '{{.SecurityOptions}}' 2>/dev/null | grep -q rootless; then
        problem "rootless Docker detected: standard (rootful) Docker is required."
    fi
    if [[ "${mode}" != "update" ]]; then
        local port
        for port in 8443 3260; do
            if port_in_use "${port}"; then
                problem "port ${port} is already in use on this machine."
            fi
        done
        if host_daemon_found; then
            problem "a tgt or mhvtl daemon is already running on the host: stop it before installing."
        fi
    fi
    if command -v mokutil >/dev/null 2>&1 && mokutil --sb-state 2>/dev/null | grep -qi 'SecureBoot enabled'; then
        problem "Secure Boot is enabled: the mhvtl kernel module could not be loaded. Disable it in the firmware."
    fi
    kernel="$(uname -r)"
    preflight_kernel_headers "${kernel}"
    if [[ "${defer}" != "defer" ]]; then
        preflight_conclude
    fi
}

https_reachable() {
    local code=""
    code="$(curl -sS --max-time 10 -o /dev/null -w '%{http_code}' "$1" 2>/dev/null)" || return 1
    [[ "${code}" =~ ^[0-9]{3}$ && "${code}" != "000" ]]
}

# preflight_resources: what a fresh installation needs. An appliance in service
# is not measured against them: it runs, and its disk holds its own data.
# MemTotal reads a little under the nominal size, hence 3.5 GiB for "4 GiB".
preflight_resources() {
    local cpus mem_kb free_kb dir
    cpus="$(nproc 2>/dev/null || echo 0)"
    if [[ "${cpus}" -lt 2 ]]; then
        problem "${cpus} CPU: at least 2 required."
    fi
    mem_kb="$(awk '/^MemTotal:/ { print $2 }' "${ROOT}/proc/meminfo" 2>/dev/null || true)"
    if [[ -z "${mem_kb}" || "${mem_kb}" -lt 3670016 ]]; then
        problem "not enough RAM ($(( ${mem_kb:-0} / 1024 )) MiB): 4 GiB required."
    fi
    dir="${ROOT}/var/lib/docker"
    while [[ ! -d "${dir}" && "${dir}" != "/" ]]; do
        dir="$(dirname "${dir}")"
    done
    free_kb="$(df -Pk "${dir}" 2>/dev/null | awk 'NR == 2 { print $4 }')"
    if [[ -z "${free_kb}" || "${free_kb}" -lt 20971520 ]]; then
        problem "not enough free space under /var/lib/docker ($(( ${free_kb:-0} / 1048576 )) GiB): 20 GiB required."
    fi
}

# preflight_sizing [defer]: resources (not in update mode) and network. Skipped
# by --skip-preflight.
preflight_sizing() {
    local defer="${1:-}" url
    if [[ "${MODE}" != "update" ]]; then
        preflight_resources
    fi
    for url in https://ghcr.io/v2/ https://deb.debian.org/ https://download.docker.com/; do
        if ! https_reachable "${url}"; then
            problem "${url%%/*}//${url#https://} unreachable over HTTPS: check the network, the proxy and the firewall."
        fi
    done
    if [[ "${defer}" != "defer" ]]; then
        preflight_conclude
    fi
}

# mark_done <step>: records a finished step, once, in the state file.
mark_done() {
    if [[ "${MODE}" == "update" ]]; then
        return
    fi
    mkdir -p "$(dirname "${STATE_FILE}")"
    if ! grep -qxF "done=$1" "${STATE_FILE}" 2>/dev/null; then
        printf 'done=%s\n' "$1" >>"${STATE_FILE}"
    fi
}

# The tools the script and the updater use that a minimal Debian may lack, as
# tool:package. curl is there when the script arrives by `curl | bash`, not
# when it is run from a file. An update generates no certificate: no openssl.
MISSING_TOOLS=()
find_missing_tools() {
    local pair needed=(curl:curl python3:python3)
    MISSING_TOOLS=()
    if [[ "${MODE}" != "update" ]]; then
        needed+=(openssl:openssl)
    fi
    for pair in "${needed[@]}"; do
        if ! command -v "${pair%%:*}" >/dev/null 2>&1; then
            MISSING_TOOLS+=("${pair}")
        fi
    done
}

# preflight_tools: a missing tool is installed after the safety checks; here
# its package must only be known to apt.
preflight_tools() {
    local pair
    for pair in "${MISSING_TOOLS[@]}"; do
        preflight_apt_lists || return 0
        if ! apt_has_candidate "${pair#*:}"; then
            problem "the tool ${pair%%:*} is missing and its package (${pair#*:}) cannot be found in the apt repositories."
        fi
    done
}

# install_missing_tools: before their first use, once the machine is accepted.
install_missing_tools() {
    local pair packages=()
    for pair in "${MISSING_TOOLS[@]}"; do
        packages+=("${pair#*:}")
        if [[ "${pair%%:*}" == "curl" ]]; then
            packages+=(ca-certificates)
        fi
    done
    log "missing tools, installed before their first use: ${packages[*]}"
    apt_install "${packages[@]}"
    PREFLIGHT_INSTALLED=" ${packages[*]}"
    for pair in "${MISSING_TOOLS[@]}"; do
        if ! command -v "${pair%%:*}" >/dev/null 2>&1; then
            fail "the tool ${pair%%:*} still cannot be found after installing the package ${pair#*:}." "${RESUME_HINT}"
        fi
    done
}

# Always run, even on resume: it is cheap and the machine may have changed.
step_preflight() {
    local mode=install
    # On resume the installer's own appliance may already hold the ports and
    # run the storage daemons: only then are those two checks skipped.
    if [[ "${MODE}" == "update" ]]; then
        mode=update
    elif [[ "${MODE}" == "resume" ]] && { container_exists "${APPLIANCE_CONTAINER}" || step_is_done host_service; }; then
        # The host storage daemons the installer itself enabled may be running.
        mode=update
    fi
    find_missing_tools
    preflight_safety "${mode}" defer
    preflight_tools
    if [[ "${#MISSING_TOOLS[@]}" -gt 0 ]]; then
        # Nothing is installed on a machine the safety checks refuse; the
        # network checks need curl, so the tools come before them.
        preflight_conclude
        install_missing_tools
    fi
    if [[ "${SKIP_PREFLIGHT}" -eq 0 ]]; then
        preflight_sizing defer
    fi
    preflight_conclude
    mark_done preflight
    if [[ "${SKIP_PREFLIGHT}" -eq 1 ]]; then
        ok "safety checks passed (sizing skipped)"
    else
        ok "machine meets the requirements"
    fi
}

# record_state <key> <value>: appends key=value; readers take the last one.
record_state() {
    mkdir -p "$(dirname "${STATE_FILE}")"
    printf '%s=%s\n' "$1" "$2" >>"${STATE_FILE}"
}

# state_value <key>: the last value recorded for the key, empty if none.
state_value() {
    local line value=""
    # An update ignores whatever state file an earlier run left behind.
    if [[ "${MODE}" != "update" && -f "${STATE_FILE}" ]]; then
        while IFS= read -r line; do
            if [[ "${line}" == "$1="* ]]; then
                value="${line#*=}"
            fi
        done <"${STATE_FILE}"
    fi
    printf '%s' "${value}"
}

step_is_done() {
    [[ "${MODE}" != "update" ]] && grep -qxF "done=$1" "${STATE_FILE}" 2>/dev/null
}

# ------------------------------------------------- credentials, docker
# Steps that run again on a resumed installation even when recorded as done:
# the machine may have changed, and credentials are never stored.
ALWAYS_RERUN=" preflight credentials summary "

# parse_site_answer: reads a JSON object on stdin, prints on separate lines
# valid (true|false), then registry, username, password or the refusal message.
parse_site_answer() {
    python3 -c '
import json, sys
try:
    answer = json.load(sys.stdin)
    if answer.get("valid") is True:
        fields = ["true", answer["registry"], answer["username"], answer["password"]]
    elif answer.get("valid") is False:
        fields = ["false", str(answer.get("message", ""))]
    else:
        raise ValueError("no valid field")
except Exception:
    sys.exit(1)
print("\n".join(fields))
'
}

# site_request_body: {"hostname","os"} on stdout, with "token" when the operator
# gave one, built from stdin lines so that the token is on no command line.
site_request_body() {
    python3 -c '
import json, sys
token, hostname, os_name = (line.rstrip("\n") for line in sys.stdin)
body = {"hostname": hostname, "os": os_name}
if token:
    body["token"] = token
print(json.dumps(body))
'
}

registry_credentials() {
    local answer="" valid="" message="" rc=0 host_name os_name
    if [[ -n "${REGISTRY_TOKEN}" ]]; then
        REGISTRY="ghcr.io"
        REGISTRY_PASSWORD="${REGISTRY_TOKEN}"
        log "registry credentials given by the operator (user ${REGISTRY_USER})"
        return
    fi
    # The image is public: without a token nothing is asked of anyone, and
    # REGISTRY stays empty so that pull_image logs in nowhere.
    if [[ -z "${TOKEN}" ]]; then
        REGISTRY=""
        log "no token: the image is downloaded without registry credentials"
        return
    fi
    host_name="$(hostname)"
    os_name="debian-$(os_release_value VERSION_ID)"
    log "exchanging the token with ${VALIDATE_URL}"
    answer="$(printf '%s\n%s\n%s\n' "${TOKEN}" "${host_name}" "${os_name}" \
        | site_request_body \
        | curl -sS --max-time 30 -X POST -H 'Content-Type: application/json' \
            --data @- "${VALIDATE_URL}" 2>/dev/null)" || rc=$?
    if [[ "${rc}" -ne 0 ]]; then
        fail "vaultcenter.io is unreachable (curl code ${rc}): check the network and the proxy." "${RESUME_HINT}"
    fi
    {
        IFS= read -r valid
        IFS= read -r message
    } < <(parse_site_answer <<<"${answer}") || fail "unreadable answer from vaultcenter.io." "${RESUME_HINT}"
    if [[ "${valid}" != "true" ]]; then
        fail "the token was refused by vaultcenter.io: ${message:-no reason given}." ""
    fi
    {
        IFS= read -r valid
        IFS= read -r REGISTRY
        IFS= read -r REGISTRY_USER
        IFS= read -r REGISTRY_PASSWORD
    } < <(parse_site_answer <<<"${answer}")
    log "credentials received from vaultcenter.io, registry ${REGISTRY}, user ${REGISTRY_USER}"
}

step_credentials() {
    # Once the image is pulled nothing needs the registry any more.
    if step_is_done image; then
        mark_done credentials
        ok "image already downloaded, no credentials needed"
        return
    fi
    registry_credentials
    mark_done credentials
    if [[ -z "${REGISTRY}" ]]; then
        ok "public image, no credentials needed"
        return
    fi
    ok "registry credentials obtained (${REGISTRY_USER}@${REGISTRY})"
}

# use_docker_config: every docker command of the installer uses its own client
# configuration, never the root user's and never /etc/vaultcenter-host.
use_docker_config() {
    mkdir -p "${DOCKER_CLIENT_DIR}"
    chmod 700 "${DOCKER_CLIENT_DIR}"
    export DOCKER_CONFIG="${DOCKER_CLIENT_DIR}"
}

# apt_call <apt-get args...>: apt-get, never interactive, waiting up to 5 minutes
# for the dpkg lock (unattended-upgrades often holds it on a fresh machine).
# Returns its status; RUN_OUTPUT holds its output.
apt_call() {
    run_logged env DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 "${APT_PROXY_ARGS[@]}" "$@"
}

# set_apt_proxy: apt reads only the lowercase http_proxy and https_proxy, while
# the script documents HTTPS_PROXY: the proxy is given to apt explicitly, each
# scheme falling back on the other's (Debian mirrors are often plain HTTP).
APT_PROXY_ARGS=()
set_apt_proxy() {
    local https_value="${HTTPS_PROXY:-${https_proxy:-}}" http_value="${HTTP_PROXY:-${http_proxy:-}}" value
    local with_password='^[A-Za-z][A-Za-z0-9+.-]*://[^:/@]*:([^@/]+)@'
    APT_PROXY_ARGS=()
    PROXY_SECRETS=()
    if [[ -z "${https_value}${http_value}" ]]; then
        return
    fi
    APT_PROXY_ARGS=(-o "Acquire::http::Proxy=${http_value:-${https_value}}"
        -o "Acquire::https::Proxy=${https_value:-${http_value}}")
    # scheme://user:password@host: the password never reaches the log.
    for value in "${https_value}" "${http_value}"; do
        if [[ "${value}" =~ ${with_password} ]]; then
            PROXY_SECRETS+=("${BASH_REMATCH[1]}")
        fi
    done
}

# apt_run <apt-get args...>: apt_call that must succeed.
apt_run() {
    if ! apt_call "$@"; then
        if grep -Eq 'Could not get lock|Unable to acquire' <<<"${RUN_OUTPUT}"; then
            fail "the apt/dpkg lock stayed held for more than 5 minutes: another process (most often unattended-upgrades, which runs after a machine is created) is installing packages. Wait for it to finish or stop it, then run again." "${RESUME_HINT}"
        fi
        fail "apt-get $1 failed (details in the log)." "${RESUME_HINT}"
    fi
}

# apt_install <packages...>: installs packages, refreshing the apt lists first
# unless they are fresh: the preflight refreshes them once per run (empty lists
# on a fresh image), and adding a repository makes them stale again.
apt_install() {
    if [[ "${APT_UPDATED}" -eq 0 ]]; then
        apt_run update
        APT_UPDATED=1
    fi
    apt_run install -y "$@"
}

install_docker_packages() {
    local version codename
    version="$(os_release_value VERSION_ID)"
    case "${version}" in
        12) codename=bookworm ;;
        13) codename=trixie ;;
        *) fail "Debian ${version} is not supported for installing Docker." "" ;;
    esac
    mkdir -p "${ROOT}/etc/apt/keyrings" "${ROOT}/etc/apt/sources.list.d"
    chmod 755 "${ROOT}/etc/apt/keyrings"
    if ! run_logged curl -fsSL --max-time 60 https://download.docker.com/linux/debian/gpg \
        -o "${ROOT}/etc/apt/keyrings/docker.asc" || [[ ! -s "${ROOT}/etc/apt/keyrings/docker.asc" ]]; then
        fail "the Docker repository key could not be downloaded from download.docker.com." "${RESUME_HINT}"
    fi
    chmod a+r "${ROOT}/etc/apt/keyrings/docker.asc"
    printf 'deb [arch=amd64 signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian %s stable\n' \
        "${codename}" >"${ROOT}/etc/apt/sources.list.d/docker.list"
    # A repository was just added: the lists the preflight refreshed do not
    # know its packages.
    APT_UPDATED=0
    apt_install docker-ce docker-ce-cli containerd.io docker-compose-plugin
    run_logged systemctl enable --now docker || fail "starting the docker service failed." "${RESUME_HINT}"
}

# configure_docker_proxy: the daemon pulls images, it does not see the
# installer's environment. Restarts it only when the drop-in changes.
configure_docker_proxy() {
    local https_proxy_value="${HTTPS_PROXY:-${https_proxy:-}}"
    local http_proxy_value="${HTTP_PROXY:-${http_proxy:-}}"
    local no_proxy_value="${NO_PROXY:-${no_proxy:-}}"
    if [[ -z "${https_proxy_value}" ]]; then
        return
    fi
    local dir="${ROOT}/etc/systemd/system/docker.service.d" file content
    file="${dir}/vaultcenter-proxy.conf"
    # systemd expands % in Environment=
    content="[Service]"$'\n'"Environment=HTTPS_PROXY=${https_proxy_value//%/%%}"$'\n'
    if [[ -n "${http_proxy_value}" ]]; then
        content+="Environment=HTTP_PROXY=${http_proxy_value//%/%%}"$'\n'
    fi
    if [[ -n "${no_proxy_value}" ]]; then
        content+="Environment=NO_PROXY=${no_proxy_value//%/%%}"$'\n'
    fi
    if [[ -f "${file}" && "$(cat "${file}")" == "${content%$'\n'}" ]]; then
        return
    fi
    if [[ "${MODE}" == "update" ]]; then
        # Restarting the daemon would bounce the running appliance and cut
        # iSCSI outside the maintenance window: only tell the operator.
        local warning="the proxy of the environment differs from ${file#"${ROOT}"}: the file has not been changed and Docker has not been restarted (that would interrupt the appliance in service). The download uses the current configuration of the daemon. To apply it later, outside backup windows: update this file, then systemctl daemon-reload && systemctl restart docker."
        warn "${warning}"
        return
    fi
    mkdir -p "${dir}"
    (umask 077 && printf '%s' "${content}" >"${file}")
    run_logged systemctl daemon-reload || fail "systemctl daemon-reload failed." "${RESUME_HINT}"
    run_logged systemctl restart docker || fail "restarting docker with the proxy failed." "${RESUME_HINT}"
}

docker_usable() {
    command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1
}

ensure_docker() {
    use_docker_config
    if docker_usable; then
        log "docker and the compose plugin are present: kept"
        DOCKER_INSTALLED_BY_US=0
    elif [[ "${MODE}" == "update" ]]; then
        fail "Docker (with the compose plugin) cannot be found or is unusable on an installed appliance: the update does not install it. Repair Docker, then run again." ""
    else
        install_docker_packages
        DOCKER_INSTALLED_BY_US=1
    fi
    configure_docker_proxy
    if ! docker_usable; then
        fail "docker (with the compose plugin) still cannot be found after the installation." "${RESUME_HINT}"
    fi
}
DOCKER_INSTALLED_BY_US=0

step_docker() {
    ensure_docker
    mark_done docker
    if [[ "${DOCKER_INSTALLED_BY_US}" -eq 1 ]]; then
        ok "Docker installed from the official repository"
    else
        ok "Docker already present, kept"
    fi
}

# ---------------------------------------------------- image and payload
registry_login() {
    local out="" rc=0
    log "\$ docker login ${REGISTRY} --username ${REGISTRY_USER} --password-stdin"
    out="$(printf '%s' "${REGISTRY_PASSWORD}" \
        | docker login "${REGISTRY}" --username "${REGISTRY_USER}" --password-stdin 2>&1)" || rc=$?
    if [[ -n "${out}" ]]; then
        log "${out}"
    fi
    log "exit ${rc}"
    LOGIN_REGISTRY="${REGISTRY}"
    if [[ "${rc}" -ne 0 ]]; then
        fail "the login to ${REGISTRY} was refused (invalid credentials or registry unreachable)." "${RESUME_HINT}"
    fi
}

# pull_image: sets IMAGE_REF (repository@sha256:...) and IMAGE_VERSION.
pull_image() {
    local tag="${VERSION:-latest}" digest="" label="" pulled=0
    use_docker_config
    if [[ -n "${REGISTRY}" ]]; then
        registry_login
    fi
    # A pull lasts minutes: on a terminal its progress is shown as it comes;
    # elsewhere one line says it began and one that it ended.
    if [[ "${FANCY}" -eq 1 ]]; then
        printf '      downloading %s (several minutes depending on bandwidth)\n' "${IMAGE_REPOSITORY}:${tag}"
        if run_shown docker pull "${IMAGE_REPOSITORY}:${tag}"; then pulled=1; fi
    else
        printf 'Downloading image %s (several minutes depending on bandwidth)...\n' "${IMAGE_REPOSITORY}:${tag}"
        if run_logged docker pull "${IMAGE_REPOSITORY}:${tag}"; then
            pulled=1
            printf 'Image download finished.\n'
        fi
    fi
    if [[ "${pulled}" -ne 1 ]]; then
        if grep -qi 'manifest unknown' <<<"${RUN_OUTPUT}"; then
            fail "version ${tag} does not exist in the registry (${IMAGE_REPOSITORY})." ""
        fi
        if [[ -z "${REGISTRY}" ]] && grep -qiE 'denied|unauthorized' <<<"${RUN_OUTPUT}"; then
            fail "the registry refused to download ${IMAGE_REPOSITORY}:${tag} without credentials: this version is not public or does not exist." ""
        fi
        fail "downloading ${IMAGE_REPOSITORY}:${tag} failed (details in the log)." "${RESUME_HINT}"
    fi
    digest="$(docker image inspect --format '{{index .RepoDigests 0}}' "${IMAGE_REPOSITORY}:${tag}" 2>/dev/null)" || digest=""
    if [[ ! "${digest}" =~ ^ghcr\.io/adminsyspro/vaultcenter@sha256:[0-9a-f]{64}$ ]]; then
        fail "the digest of the downloaded image cannot be found or is unexpected (${digest:-empty})." "${RESUME_HINT}"
    fi
    IMAGE_REF="${digest}"
    label="$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "${IMAGE_REF}" 2>/dev/null)" || label=""
    if [[ -n "${VERSION}" && "${label}" != "${VERSION}" ]]; then
        fail "the requested version is ${VERSION} but the image carries the label ${label:-empty}." ""
    fi
    if [[ ! "${label}" =~ ${VERSION_REGEX} ]]; then
        fail "the image carries no valid version label, X.Y.Z or X.Y.Z-<pre-release> (${label:-empty})." ""
    fi
    IMAGE_VERSION="${label}"
    registry_logout
}

step_image() {
    pull_image
    # An update records nothing: a later run must never read this image back.
    if [[ "${MODE}" != "update" ]]; then
        record_state image "${IMAGE_REF}"
        record_state version "${IMAGE_VERSION}"
    fi
    mark_done image
    ok "image ${IMAGE_VERSION} (${IMAGE_REF#*@})"
}

# extract_payload: copies the host files out of the image, through a container
# that is created, never started, and always removed.
extract_payload() {
    local container="vaultcenter-install-payload" dest="${PAYLOAD_ROOT}/${IMAGE_VERSION}" ok_copy=1
    use_docker_config
    mkdir -p "${dest}"
    # A fixed name: a leftover from a killed run is cleared first.
    run_logged docker rm -f "${container}" || true
    PAYLOAD_CONTAINER="${container}"
    if run_logged docker create --name "${container}" "${IMAGE_REF}"; then
        run_logged docker cp "${container}:/usr/local/share/vaultcenter/host" "${dest}/" || ok_copy=0
        if [[ "${ok_copy}" -eq 1 ]]; then
            run_logged docker cp "${container}:/usr/share/doc/mhvtl/source.tar.gz" "${dest}/mhvtl-source.tar.gz" || ok_copy=0
        fi
    else
        ok_copy=0
    fi
    remove_payload_container
    if [[ "${ok_copy}" -ne 1 || ! -d "${dest}/host" || ! -f "${dest}/mhvtl-source.tar.gz" ]]; then
        fail "extracting the host files from the image failed (details in the log)." "${RESUME_HINT}"
    fi
}

step_payload() {
    if [[ -z "${IMAGE_REF}" || -z "${IMAGE_VERSION}" ]]; then
        fail "the reference image is unknown (incomplete installation state)." ""
    fi
    extract_payload
    mark_done payload
    ok "host files extracted to ${PAYLOAD_ROOT#"${ROOT}"}/${IMAGE_VERSION}"
}

# --------------------------------------------------------- kernel module
# Arguments that point dkms at the test root; empty on a real machine, where
# dkms keeps its own defaults.
DKMS_TREE_ARGS=()

# dkms_installed <version> <kernel>: this package version is installed for that kernel.
dkms_installed() {
    local status=""
    status="$(dkms status -m mhvtl -v "$1" -k "$2" "${DKMS_TREE_ARGS[@]}" 2>/dev/null)" || true
    grep -q ': installed' <<<"${status}"
}

# kernels_with_headers <running kernel>: every kernel whose headers are
# installed, one per line, the running one first. A kernel that is installed
# but not booted yet needs the module as much as the running one.
kernels_with_headers() {
    local running="$1" dir name
    printf '%s\n' "${running}"
    for dir in "${ROOT}"/lib/modules/*/build; do
        [[ -e "${dir}" ]] || continue
        name="${dir%/build}"
        name="${name##*/}"
        if [[ "${name}" != "${running}" ]]; then
            printf '%s\n' "${name}"
        fi
    done
}

# build_kernel_module <version> <kernel> <running kernel>: dkms build then
# install. A failure for the running kernel stops the installation; for another
# kernel it is a warning (returns 1).
build_kernel_module() {
    local version="$1" kernel="$2" running="$3" build_log later
    build_log="${ROOT}/var/lib/dkms/mhvtl/${version}/build/make.log"
    later="this kernel is installed but is not the running one: when the machine boots on this kernel, the vaultcenter-mhvtl service will try to rebuild the module; to do it beforehand, run dkms install -m mhvtl -v ${version} -k ${kernel}"
    if ! run_logged dkms build -m mhvtl -v "${version}" -k "${kernel}" "${DKMS_TREE_ARGS[@]}"; then
        if [[ -r "${build_log}" ]]; then
            log "$(tail -n 40 "${build_log}")"
        fi
        if [[ "${kernel}" == "${running}" ]]; then
            fail "building the mhvtl module failed for kernel ${kernel}. DKMS build log: ${build_log}" "${RESUME_HINT}"
        fi
        warn "building the mhvtl module failed for kernel ${kernel} (DKMS build log: ${build_log}); ${later}."
        return 1
    fi
    if ! run_logged dkms install -m mhvtl -v "${version}" -k "${kernel}" "${DKMS_TREE_ARGS[@]}"; then
        if [[ "${kernel}" == "${running}" ]]; then
            fail "dkms install failed (details in the log)." "${RESUME_HINT}"
        fi
        warn "dkms install failed for kernel ${kernel} (details in the log); ${later}."
        return 1
    fi
}

# install_kernel_modules <version> <running kernel> <archive> <helper> <dkms.conf>:
# builds and installs the module for every kernel that has its headers and not
# this version yet. Sets MODULE_KERNELS to the number of kernels that have it.
MODULE_KERNELS=0
install_kernel_modules() {
    local version="$1" running="$2" archive="$3" helper="$4" conf="$5" kernel missing=()
    MODULE_KERNELS=0
    while IFS= read -r kernel; do
        if dkms_installed "${version}" "${kernel}"; then
            MODULE_KERNELS=$((MODULE_KERNELS + 1))
        else
            missing+=("${kernel}")
        fi
    done < <(kernels_with_headers "${running}")
    if [[ "${#missing[@]}" -eq 0 ]]; then
        return
    fi
    bash "${helper}" "${archive}" "${ROOT:-/}" "${conf}" >/dev/null \
        || fail "the DKMS sources could not be prepared in ${ROOT}/usr/src." "${RESUME_HINT}"
    # A resumed installation may already have added this exact version.
    if ! run_logged dkms add -m mhvtl -v "${version}" "${DKMS_TREE_ARGS[@]}" \
        && ! grep -qi 'already' <<<"${RUN_OUTPUT}"; then
        fail "dkms add failed (details in the log)." "${RESUME_HINT}"
    fi
    for kernel in "${missing[@]}"; do
        if build_kernel_module "${version}" "${kernel}" "${running}"; then
            MODULE_KERNELS=$((MODULE_KERNELS + 1))
        fi
    done
}

# install_module_guard <host dir>: the unit that, at boot, rebuilds and loads the
# module when the booted kernel has none (a kernel upgrade whose headers were
# missing), before Docker starts.
install_module_guard() {
    local from="$1/vaultcenter-mhvtl.service" to="${ROOT}/etc/systemd/system/vaultcenter-mhvtl.service"
    if [[ ! -f "${from}" ]]; then
        fail "the file vaultcenter-mhvtl.service is missing from the extracted host files." "${RESUME_HINT}"
    fi
    mkdir -p "${ROOT}/etc/systemd/system"
    cp "${from}" "${to}"
    chmod 644 "${to}"
    run_logged systemctl daemon-reload || fail "systemctl daemon-reload failed." "${RESUME_HINT}"
    run_logged systemctl enable vaultcenter-mhvtl.service \
        || fail "enabling vaultcenter-mhvtl.service failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
}

# install_persistent_settings <host dir>: what keeps the module and the
# console/udev settings across reboots.
install_persistent_settings() {
    local host="$1" pair file_name from to
    mkdir -p "${ROOT}/etc/modules-load.d" "${ROOT}/etc/modprobe.d" "${ROOT}/etc/sysctl.d" "${ROOT}/etc/udev/rules.d"
    printf 'mhvtl\nsg\n' >"${ROOT}/etc/modules-load.d/vaultcenter.conf"
    chmod 644 "${ROOT}/etc/modules-load.d/vaultcenter.conf"
    for pair in "vaultcenter-mhvtl.conf:etc/modprobe.d" "99-vaultcenter-console.conf:etc/sysctl.d" "99-vaultcenter.rules:etc/udev/rules.d"; do
        file_name="${pair%%:*}"
        from="${host}/${file_name}"
        to="${ROOT}/${pair#*:}/${file_name}"
        if [[ ! -f "${from}" ]]; then
            fail "the file ${file_name} is missing from the extracted host files." "${RESUME_HINT}"
        fi
        cp "${from}" "${to}"
        chmod 644 "${to}"
    done
}

step_kernel_module() {
    local dest="${PAYLOAD_ROOT}/${IMAGE_VERSION}" archive helper kernel version fingerprint loaded=0 meta others
    local packages=()
    archive="${dest}/mhvtl-source.tar.gz"
    helper="${dest}/host/prepare-dkms-source.sh"
    kernel="$(uname -r)"
    if [[ ! -f "${archive}" || ! -f "${helper}" || ! -f "${dest}/host/dkms.conf" ]]; then
        fail "the mhvtl module sources are missing from ${dest#"${ROOT}"} (incomplete extraction)." "${RESUME_HINT}"
    fi
    if [[ -n "${ROOT}" ]]; then
        DKMS_TREE_ARGS=(--sourcetree "${ROOT}/usr/src" --dkmstree "${ROOT}/var/lib/dkms")
    fi
    version="$(bash "${helper}" --version "${archive}")" || fail "the fingerprint of the mhvtl sources could not be computed." ""
    if [[ -e "${ROOT}/sys/module/mhvtl" ]]; then
        loaded=1
        if ! dkms_installed "${version}" "${kernel}"; then
            refuse "an mhvtl module of another origin is already loaded (it does not come from this installer). Unload it (modprobe -r mhvtl) or remove it, then run again: the module installed here would replace a module that another backup setup may be using."
        fi
    fi
    # The versioned package serves the running kernel; the package of the
    # flavour brings the headers of every later kernel, for DKMS to rebuild.
    packages=(dkms build-essential "linux-headers-${kernel}")
    meta="$(kernel_headers_metapackage "${kernel}")"
    if [[ -n "${meta}" ]]; then
        packages+=("${meta}")
    else
        warn "kernel ${kernel} (flavour \"${kernel##*-}\") is not of a Debian flavour this program knows (amd64, cloud-amd64, rt-amd64): no package will bring its headers along. After every kernel update, install the linux-headers package of the new kernel before rebooting."
    fi
    apt_install "${packages[@]}"
    install_kernel_modules "${version}" "${kernel}" "${archive}" "${helper}" "${dest}/host/dkms.conf"
    if [[ "${loaded}" -eq 0 ]]; then
        run_logged modprobe mhvtl opts=0 || fail "modprobe mhvtl failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
    fi
    run_logged modprobe sg || fail "modprobe sg failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
    install_persistent_settings "${dest}/host"
    install_module_guard "${dest}/host"
    run_logged sysctl --load "${ROOT}/etc/sysctl.d/99-vaultcenter-console.conf" \
        || fail "sysctl --load failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
    run_logged udevadm control --reload-rules || fail "udevadm control failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
    run_logged udevadm trigger --subsystem-match=scsi_generic \
        || fail "udevadm trigger failed: ${RUN_OUTPUT}" "${RESUME_HINT}"
    fingerprint="$(sha256sum "${archive}")"
    mkdir -p "${ROOT}/var/lib/vaultcenter-install"
    printf '%s\n' "${fingerprint%% *}" >"${ROOT}/var/lib/vaultcenter-install/mhvtl.sha256"
    mark_done kernel_module
    others=""
    if [[ "${MODULE_KERNELS}" -gt 1 ]]; then
        others=" and $((MODULE_KERNELS - 1)) other installed kernel(s)"
    fi
    ok "mhvtl module ${version} installed for kernel ${kernel}${others}"
}


# ------------------------------------------------ host service, configuration
# prepare_compose_source <payload host dir>: the payload's compose.yaml with the
# two relative bind sources made absolute, since install.py refuses to run when
# /etc/vaultcenter-host exists and the files must live there. The paths are
# logical (Docker reads them), not prefixed by ${ROOT}.
prepare_compose_source() {
    local source="$1/compose.yaml" dir="${ROOT}/var/lib/vaultcenter-install/compose" content=""
    [[ -f "${source}" ]] || fail "compose.yaml is missing from the extracted host files." "${RESUME_HINT}"
    content="$(cat "${source}")"
    content="${content//source: .\/config\/cold-vtl$'\n'/source: \/etc\/vaultcenter-host\/config\/cold-vtl$'\n'}"
    content="${content//source: .\/config\/targets.conf$'\n'/source: \/etc\/vaultcenter-host\/config\/targets.conf$'\n'}"
    if [[ "${content}" == *"./config/"* ]]; then
        fail "compose.yaml does not have the expected format (./config mounts not found)." ""
    fi
    mkdir -p "${dir}"
    printf '%s\n' "${content}" >"${dir}/compose.yaml"
    COMPOSE_SOURCE="${dir}/compose.yaml"
}
COMPOSE_SOURCE=""

step_host_service() {
    local installer="${PAYLOAD_ROOT}/${IMAGE_VERSION}/host/install.py" detail=""
    if [[ -z "${IMAGE_REF}" || -z "${IMAGE_VERSION}" ]]; then
        fail "the reference image is unknown (incomplete installation state)." ""
    fi
    if [[ -f "${HOST_DIR}/compose.json" ]]; then
        # This step is not recorded as done, and compose.json is there.
        fail "${HOST_LEFTOVER_CAUSE} See \"Resuming after an interruption\" in $(doc_path)." "${HOST_LEFTOVER_RESUME}"
    fi
    if [[ -d "${HOST_DIR}" && ! -f "${HOST_DIR}/compose.json" ]]; then
        fail "${HOST_DIR#"${ROOT}"} exists without compose.json: an earlier run of the host installer stopped part-way. Nothing has been deleted." \
            "delete the directory ${HOST_DIR#"${ROOT}"} yourself (rm -r ${HOST_DIR#"${ROOT}"}), then ${RESUME_HINT}"
    fi
    [[ -f "${installer}" ]] || fail "install.py is missing from the extracted host files." "${RESUME_HINT}"
    use_docker_config
    prepare_compose_source "$(dirname "${installer}")"
    # install.py sets VAULTCENTER_IMAGE itself from --image.
    if ! run_logged env -u VAULTCENTER_IMAGE python3 "${installer}" --compose "${COMPOSE_SOURCE}" \
        --new-appliance --isolated --image "${IMAGE_REF}"; then
        detail="${RUN_OUTPUT##*$'\n'}"
        fail "installing the host storage service failed (${detail:-no details}); details in the log." "${RESUME_HINT}"
    fi
    if [[ ! -f "${HOST_DIR}/compose.json" ]]; then
        fail "the host installer did not produce ${HOST_DIR#"${ROOT}"}/compose.json." "${RESUME_HINT}"
    fi
    mark_done host_service
    ok "storage service installed (isolated mode)"
}

# The name the certificate and the URL carry: the full host name when it is a
# real one, else the address of the default route.
PUBLIC_NAME=""
PUBLIC_NAME_FQDN=""
PUBLIC_IS_ADDRESS=0
resolve_public_name() {
    local name=""
    name="$(hostname -f 2>/dev/null || true)"
    name="${name//[$' \t\r\n']/}"
    case "${name}" in
        "" | localhost | localhost.localdomain | *.localhost | *.localdomain) name="" ;;
        *.*) ;;
        *) name="" ;;
    esac
    if [[ -n "${name}" ]]; then
        PUBLIC_NAME="${name}"
        PUBLIC_NAME_FQDN="${name}"
        PUBLIC_IS_ADDRESS=0
        return
    fi
    PUBLIC_NAME="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{ for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit } }')" || PUBLIC_NAME=""
    if [[ -z "${PUBLIC_NAME}" ]]; then
        fail "the full name of the machine is unusable and its IP address could not be determined (ip route get)." ""
    fi
    PUBLIC_IS_ADDRESS=1
}

# subject_alt_names: DNS:<full name> (when there is one) and IP:<address> for
# every global address of the machine, without duplicates.
subject_alt_names() {
    local entries=() entry address seen=" "
    if [[ -n "${PUBLIC_NAME_FQDN}" ]]; then
        entries+=("DNS:${PUBLIC_NAME_FQDN}")
    fi
    while IFS= read -r address; do
        if [[ -n "${address}" ]]; then
            entries+=("IP:${address}")
        fi
    done < <({
        ip -o addr show scope global 2>/dev/null \
            | awk '$3 == "inet" || $3 == "inet6" { sub(/\/.*/, "", $4); print $4 }'
        if [[ "${PUBLIC_IS_ADDRESS}" -eq 1 ]]; then printf '%s\n' "${PUBLIC_NAME}"; fi
    })
    local joined=""
    for entry in "${entries[@]}"; do
        if [[ "${seen}" != *" ${entry} "* ]]; then
            seen+="${entry} "
            joined+="${joined:+,}${entry}"
        fi
    done
    printf '%s' "${joined}"
}

# write_auth_env <example> <target>: the payload's example with the public URL.
write_auth_env() {
    local line
    {
        while IFS= read -r line || [[ -n "${line}" ]]; do
            if [[ "${line}" == COLD_VTL_PUBLIC_URL=* ]]; then
                printf 'COLD_VTL_PUBLIC_URL=https://%s:8443\n' "${PUBLIC_NAME}"
            else
                printf '%s\n' "${line}"
            fi
        done <"$1"
    } >"$2"
}

# json_field <file> <key>: a top-level field of a JSON object (booleans as
# true/false); fails when the file is unreadable or not an object.
json_field() {
    python3 -I -c '
import json, sys
try:
    value = json.load(open(sys.argv[1], encoding="utf-8")).get(sys.argv[2])
except Exception:
    sys.exit(1)
print(str(value).lower() if isinstance(value, bool) else "" if value is None else value)
' "$1" "$2"
}

# install_isolation_receipt <payload host dir>: installs the receipt the payload
# ships when it qualifies the payload's own vtl_runtime.py; otherwise installs
# nothing and records that this version has none.
install_isolation_receipt() {
    local receipt="$1/isolation-qualified.json" runtime="$1/vtl_runtime.py" state_dir="" declared="" actual=""
    if [[ -f "${receipt}" && -f "${runtime}" ]]; then
        declared="$(json_field "${receipt}" runtime_sha256 2>/dev/null)" || declared=""
        actual="$(sha256sum "${runtime}")"
        actual="${actual%% *}"
        if [[ -n "${declared}" && "${declared}" == "${actual}" \
            && "$(json_field "${receipt}" isolation_verified 2>/dev/null || true)" == "true" ]]; then
            state_dir="$(json_field "${HOST_DIR}/storage.json" state_dir 2>/dev/null)" || state_dir=""
            if [[ -z "${state_dir}" ]]; then
                fail "state_dir cannot be found in ${HOST_DIR#"${ROOT}"}/storage.json: the isolation receipt could not be installed." "${RESUME_HINT}"
            fi
            # The path is logical; a test root may already be part of it.
            if [[ -n "${ROOT}" && "${state_dir}" != "${ROOT}/"* ]]; then
                state_dir="${ROOT}${state_dir}"
            fi
            cp "${receipt}" "${state_dir}/isolation-qualified.json" \
                || fail "the isolation receipt could not be copied to ${state_dir#"${ROOT}"}." "${RESUME_HINT}"
            chmod 600 "${state_dir}/isolation-qualified.json"
            record_state isolation_receipt installed
            log "isolation receipt installed in ${state_dir#"${ROOT}"}"
            return
        fi
        log "the isolation receipt of the image does not match its vtl_runtime.py: not installed"
    else
        log "the image provides no isolation receipt"
    fi
    record_state isolation_receipt missing
}

step_configuration() {
    local config="${HOST_DIR}/config" cold="${HOST_DIR}/config/cold-vtl" example san old_umask
    example="${PAYLOAD_ROOT}/${IMAGE_VERSION}/host/auth.env.example"
    [[ -f "${example}" ]] || fail "auth.env.example is missing from the extracted host files." "${RESUME_HINT}"
    resolve_public_name
    old_umask="$(umask)"
    umask 077
    mkdir -p "${cold}/tls" "${cold}/maintenance"
    chmod 700 "${config}" "${cold}" "${cold}/tls" "${cold}/maintenance"
    # Existing files are the operator's (a resumed run, an edited file): kept.
    if [[ ! -f "${cold}/auth.env" ]]; then
        write_auth_env "${example}" "${cold}/auth.env"
    fi
    chmod 600 "${cold}/auth.env"
    if [[ ! -f "${config}/targets.conf" ]]; then
        printf '# VTLs are created in the VaultCenter console; this file is not read in isolated mode.\n' >"${config}/targets.conf"
    fi
    chmod 600 "${config}/targets.conf"
    if [[ ! -f "${cold}/tls/fullchain.pem" || ! -f "${cold}/tls/privkey.pem" ]]; then
        san="$(subject_alt_names)"
        if ! run_logged openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
            -subj "/CN=${PUBLIC_NAME}" -addext "subjectAltName=${san}" \
            -keyout "${cold}/tls/privkey.pem" -out "${cold}/tls/fullchain.pem"; then
            umask "${old_umask}"
            fail "generating the certificate failed: ${RUN_OUTPUT##*$'\n'}" "${RESUME_HINT}"
        fi
    fi
    umask "${old_umask}"
    chmod 600 "${cold}/tls/privkey.pem"
    chmod 644 "${cold}/tls/fullchain.pem"
    install_isolation_receipt "${PAYLOAD_ROOT}/${IMAGE_VERSION}/host"
    record_state public_name "${PUBLIC_NAME}"
    record_state public_is_address "${PUBLIC_IS_ADDRESS}"
    mark_done configuration
    ok "configuration written, self-signed certificate for ${PUBLIC_NAME}"
}

# ---------------------------------------------- administrator, start, summary
# new_password: 24 characters of [A-Za-z0-9] from the kernel's random source.
new_password() {
    local password=""
    # head closes the pipe early: tr dies of SIGPIPE, which is expected.
    password="$(LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c 24)" || true
    printf '%s' "${password}"
}

step_administrator() {
    local out="" rc=0
    use_docker_config
    ADMIN_PASSWORD="$(new_password)"
    if [[ ${#ADMIN_PASSWORD} -ne 24 ]]; then
        ADMIN_PASSWORD=""
        fail "the administrator password could not be generated." "${RESUME_HINT}"
    fi
    log "\$ docker compose -f ${HOST_DIR}/compose.json run --rm -T --entrypoint /usr/local/bin/cold-vtld appliance create-user admin --role admin (password on standard input)"
    out="$(printf '%s\n' "${ADMIN_PASSWORD}" \
        | docker compose -f "${HOST_DIR}/compose.json" run --rm -T --entrypoint /usr/local/bin/cold-vtld \
            appliance create-user admin --role admin 2>&1)" || rc=$?
    if [[ -n "${out}" ]]; then
        log "${out}"
    fi
    log "exit ${rc}"
    if [[ "${rc}" -ne 0 ]] && grep -q 'username already exists' <<<"${out}"; then
        # An earlier run created the account and stopped before recording it
        # (cold-vtld answers "username already exists"): the step is done,
        # with a password this run does not know.
        ADMIN_PASSWORD=""
        log "the admin account already exists: password unchanged, unknown to this run"
        mark_done administrator
        ok "admin account already present (password unchanged)"
        return
    fi
    if [[ "${rc}" -ne 0 ]]; then
        ADMIN_PASSWORD=""
        fail "creating the administrator account failed (details in the log)." "${RESUME_HINT}"
    fi
    mark_done administrator
    ok "admin account created"
}

# positive_integer_or <value> <default>: test knobs come from the environment.
positive_integer_or() {
    if [[ "$1" =~ ^[0-9]+$ ]]; then
        printf '%s' "$1"
    else
        printf '%s' "$2"
    fi
}

# wait_healthy: polls the container health, 5 s apart, for at most 5 minutes
# (VAULTCENTER_INSTALL_HEALTH_INTERVAL / _ATTEMPTS exist for the tests).
wait_healthy() {
    local interval attempts attempt health=""
    interval="$(positive_integer_or "${VAULTCENTER_INSTALL_HEALTH_INTERVAL:-}" 5)"
    attempts="$(positive_integer_or "${VAULTCENTER_INSTALL_HEALTH_ATTEMPTS:-}" 60)"
    for ((attempt = 1; attempt <= attempts; attempt++)); do
        health="$(docker inspect --format '{{.State.Health.Status}}' "${APPLIANCE_CONTAINER}" 2>/dev/null)" || health=""
        log "health of ${APPLIANCE_CONTAINER}: ${health:-unknown} (attempt ${attempt}/${attempts})"
        if [[ "${health}" == "healthy" ]]; then
            return 0
        fi
        if [[ "${attempt}" -lt "${attempts}" ]]; then
            sleep "${interval}"
        fi
    done
    return 1
}

step_start() {
    local compose="${HOST_DIR}/compose.json" logical answer="" version=""
    logical="${compose#"${ROOT}"}"
    local look="to see the logs: docker compose -f ${logical} logs --tail=100; then"
    local lost=""
    if [[ -n "${ADMIN_PASSWORD}" ]]; then
        # The password exists only in this process: a failure here loses it.
        lost=" The password of the admin account, just created, will not be shown when resuming (it is written nowhere): after resuming, reset it with the procedure in \"The administrator account\" in $(doc_path)."
    fi
    use_docker_config
    run_logged docker compose -f "${compose}" up -d \
        || fail "docker compose up -d failed (details in the log).${lost}" "${look} ${RESUME_HINT}"
    if ! wait_healthy; then
        fail "the appliance did not become healthy within 5 minutes. Nothing has been deleted.${lost}" "${look} ${RESUME_HINT}"
    fi
    # --noproxy: the API is on the loopback, never behind HTTPS_PROXY.
    answer="$(curl -fsSk --noproxy '*' --max-time 10 https://127.0.0.1:8443/health 2>/dev/null)" || answer=""
    version="$(python3 -c 'import json, sys; print(json.load(sys.stdin)["version"])' <<<"${answer}" 2>/dev/null)" || version=""
    if [[ "${version}" != "${IMAGE_VERSION}" ]]; then
        fail "/health reports version ${version:-unknown} instead of ${IMAGE_VERSION}. Nothing has been deleted.${lost}" "${look} ${RESUME_HINT}"
    fi
    mark_done start
    ok "appliance healthy, version ${version}"
}

# print_summary: on the terminal only. It uses printf, which never reaches the
# log; the log gets a copy without the password.
print_summary() {
    local fingerprint="" name url cert="${HOST_DIR}/config/cold-vtl/tls/fullchain.pem"
    fingerprint="$(openssl x509 -noout -fingerprint -sha256 -in "${cert}" 2>/dev/null)" \
        || fail "the fingerprint of the certificate could not be read." "${RESUME_HINT}"
    fingerprint="${fingerprint#*=}"
    name="$(state_value public_name)"
    url="https://${name}:8443"
    log "summary: ${url}, admin account, fingerprint ${fingerprint} (password shown on the terminal only)"
    printf '\n%s============================================================%s\n' "${BRAND}" "${RESET}"
    printf '  %sVaultCenter is installed%s\n\n' "${BOLD}" "${RESET}"
    printf '  Address:              %s\n' "${url}"
    printf '  Account:              admin\n'
    if [[ -n "${ADMIN_PASSWORD}" ]]; then
        printf '  Password:             %s\n' "${ADMIN_PASSWORD}"
        printf '                        (shown only once: write it down now)\n'
    else
        printf '  Password:             unknown (the account already existed, created during an earlier run).\n'
        printf '                        To reset it: "The administrator account" in\n'
        printf '                        %s\n' "$(doc_path)"
    fi
    printf '  SHA-256 fingerprint:  %s\n' "${fingerprint}"
    printf '                        (self-signed certificate: compare it in your browser)\n\n'
    printf '  VTLs are created in the console, after logging in.\n'
    if [[ "$(state_value isolation_receipt)" == "missing" ]]; then
        printf '  This version provides no isolation receipt: S3 exports stay refused\n'
        printf '  by the appliance until a receipt is installed (see "The isolation receipt" in\n'
        printf '  %s).\n' "$(doc_path)"
    fi
    if [[ "$(state_value public_is_address)" == "1" ]]; then
        printf '  The full name of the machine is unusable: an IP address (%s) serves as\n' "${name}"
        printf '  the access address and is in the certificate.\n'
    fi
    printf '  Log:                  /var/log/vaultcenter-install.log\n'
    printf '%s============================================================%s\n' "${BRAND}" "${RESET}"
}

# ------------------------------------------------------------------ update
# What the updater answered (see run_updater): the JSON result of its last line.
UPDATE_RESULT="" UPDATE_FROM="" UPDATE_TO="" UPDATE_BACKUP="" UPDATE_STEP=""
UPDATE_CONFIRMABLE="false"
UPDATE_REASONS=()
UPDATE_WARNINGS=()
# The VTLs the updater put in maintenance that are still there, as
# "<name>: <reason>" (the JSON's still_in_maintenance).
UPDATE_STILL=()
UPDATE_RC=0
UPDATE_EXIT=0
UPDATER_OUTPUT=""

# parse_updater_result <stdout file>: reads the last line as the JSON result.
parse_updater_result() {
    local line="" parsed="" entry=""
    UPDATE_RESULT="" UPDATE_FROM="" UPDATE_TO="" UPDATE_BACKUP="" UPDATE_STEP=""
    UPDATE_CONFIRMABLE="false"
    UPDATE_REASONS=()
    UPDATE_WARNINGS=()
    UPDATE_STILL=()
    line="$(tail -n 1 "$1")"
    parsed="$(python3 -I -c '
import json, sys
try:
    answer = json.loads(sys.stdin.read())
    if answer["result"] not in ("updated", "unchanged", "refused", "rolled_back", "rollback_failed"):
        raise ValueError("unknown result")
except Exception:
    sys.exit(1)

def one(value):
    return " ".join(str(value if value is not None else "").split())

def many(name):
    value = answer.get(name)
    return value if isinstance(value, list) else []

def still(item):
    if not isinstance(item, dict):
        return one(item)
    name = one(item.get("name") or item.get("vtl") or "VTL")
    reason = one(item.get("reason"))
    return name + (": " + reason if reason else "")

lines = ["result=" + answer["result"], "from=" + one(answer.get("from")), "to=" + one(answer.get("to")),
         "backup=" + one(answer.get("backup")), "step=" + one(answer.get("step")),
         "confirmable=" + ("true" if answer.get("confirmable") is True else "false")]
lines += ["reason=" + one(item) for item in many("reasons")]
lines += ["warning=" + one(item) for item in many("warnings")]
lines += ["still=" + still(item) for item in many("still_in_maintenance")]
print("\n".join(lines))
' <<<"${line}" 2>/dev/null)" || return 1
    while IFS= read -r entry; do
        case "${entry}" in
            result=*) UPDATE_RESULT="${entry#*=}" ;;
            from=*) UPDATE_FROM="${entry#*=}" ;;
            to=*) UPDATE_TO="${entry#*=}" ;;
            backup=*) UPDATE_BACKUP="${entry#*=}" ;;
            step=*) UPDATE_STEP="${entry#*=}" ;;
            confirmable=*) UPDATE_CONFIRMABLE="${entry#*=}" ;;
            reason=*) UPDATE_REASONS+=("${entry#*=}") ;;
            warning=*) UPDATE_WARNINGS+=("${entry#*=}") ;;
            still=*) UPDATE_STILL+=("${entry#*=}") ;;
        esac
    done <<<"${parsed}"
}

# While the updater runs, and until its result is shown, INT, TERM and HUP do
# not stop the installer: the updater receives them too (Ctrl-C reaches the
# whole process group), rolls back by itself and then reports. Leaving at once
# would lose that report. The signal is recorded here and told in the summary.
DEFER_SIGNALS=0
DEFERRED_SIGNAL=""

# on_signal <name> <exit code>: the handler of INT, TERM and HUP.
on_signal() {
    if [[ "${DEFER_SIGNALS}" -eq 1 ]]; then
        DEFERRED_SIGNAL="$1"
        log "signal $1 received during the update: waiting for the result of the updater"
        return 0
    fi
    exit "$2"
}

# run_updater <0|1>: runs the updater of the target version, with --yes when
# asked. Its progress (stderr) is shown and logged as it comes; its stdout is
# kept apart: the last line is the JSON result.
run_updater() {
    local yes="$1" payload="${PAYLOAD_ROOT}/${IMAGE_VERSION}" line="" rc=0
    local args=(--image "${IMAGE_REF}" --version "${IMAGE_VERSION}" --payload "${payload}")
    if [[ "${yes}" -eq 1 ]]; then args+=(--yes); fi
    if [[ "${RESUME_ROLLBACK}" -eq 1 ]]; then args+=(--resume-rollback); fi
    if [[ "${FORCE_ROLLBACK}" -eq 1 ]]; then args+=(--force-rollback); fi
    mkdir -p "${ROOT}/var/lib/vaultcenter-install"
    UPDATER_OUTPUT="$(mktemp "${ROOT}/var/lib/vaultcenter-install/updater-output.XXXXXX")"
    log "\$ python3 ${payload}/host/update.py ${args[*]}"
    DEFER_SIGNALS=1
    if env VAULTCENTER_INSTALL_ROOT="${ROOT}" python3 "${payload}/host/update.py" "${args[@]}" 2>&1 >"${UPDATER_OUTPUT}" \
        | {
            # The reader must outlive a signal sent to the whole group: the
            # updater goes on, and what it says must still be shown and logged.
            trap '' INT TERM HUP
            while IFS= read -r line || [[ -n "${line}" ]]; do
                log "update: ${line}"
                printf '%s\n' "${line}" >&2 || true
            done
        }; then
        rc=0
    else
        rc="${PIPESTATUS[0]}"
    fi
    UPDATE_RC="${rc}"
    log "exit ${rc}"
    log "update result: $(tail -n 1 "${UPDATER_OUTPUT}")"
    if ! parse_updater_result "${UPDATER_OUTPUT}"; then
        rm -f "${UPDATER_OUTPUT}"
        UPDATER_OUTPUT=""
        fail "the updater gave no readable result (exit code ${rc}). Check the state of the appliance and read the log." ""
    fi
    rm -f "${UPDATER_OUTPUT}"
    UPDATER_OUTPUT=""
}

# update_exit_code: the exit code of the installer, from the JSON result and the
# updater's own exit code; when they disagree the more severe one wins.
update_exit_code() {
    local expected=0 actual="${UPDATE_RC}"
    case "${UPDATE_RESULT}" in
        updated | unchanged) expected=0 ;;
        refused) expected=3 ;;
        rolled_back) expected=5 ;;
        rollback_failed) expected=6 ;;
    esac
    case "${actual}" in
        0 | 3 | 5 | 6) ;;
        *) actual=4 ;;
    esac
    UPDATE_EXIT="${expected}"
    if [[ "${actual}" -gt "${expected}" ]]; then
        UPDATE_EXIT="${actual}"
    fi
    if [[ "${actual}" -ne "${expected}" ]]; then
        log "the announced result (${UPDATE_RESULT}, code ${expected}) and the exit code of the updater (${UPDATE_RC}) do not agree: the more severe one is kept (${UPDATE_EXIT})"
        printf 'Warning: the result announced by the update (%s) and its exit code (%s) do not agree; the more severe code (%s) is kept.\n' \
            "${UPDATE_RESULT}" "${UPDATE_RC}" "${UPDATE_EXIT}" >&2
    fi
}

step_update() {
    local yes="${ASSUME_YES}" reason
    if [[ -z "${IMAGE_REF}" || -z "${IMAGE_VERSION}" ]]; then
        fail "the target image is unknown." ""
    fi
    if [[ ! -f "${PAYLOAD_ROOT}/${IMAGE_VERSION}/host/update.py" ]]; then
        fail "update.py is missing from the extracted host files." ""
    fi
    use_docker_config
    run_updater "${yes}"
    if [[ "${UPDATE_RESULT}" == "refused" && "${yes}" -eq 0 && "${UPDATE_CONFIRMABLE}" == "true" \
        && -z "${DEFERRED_SIGNAL}" ]] && has_tty; then
        for reason in "${UPDATE_REASONS[@]}"; do
            printf '%s\n' "${reason}" >&2
        done
        # At the question nothing runs: a signal stops the installer at once.
        DEFER_SIGNALS=0
        if ask_yes "No backup job is using these libraries right now: do you confirm?"; then
            log "the operator confirms that no backup is running: calling again with --yes"
            run_updater 1
        fi
        DEFER_SIGNALS=1
    fi
    update_exit_code
    # The step line says what happened; only a success gets the success mark.
    case "${UPDATE_RESULT}" in
        updated) ok "update from ${UPDATE_FROM} to ${UPDATE_TO} finished" ;;
        unchanged) ok "already at version ${UPDATE_TO}" ;;
        refused) not_ok "update refused" ;;
        rolled_back) not_ok "update cancelled, previous version restored" ;;
        rollback_failed) not_ok "rollback failed" ;;
    esac
}

# print_update_summary: the box of an update, by result. Never a password.
print_update_summary() {
    local line
    printf '\n%s============================================================%s\n' "${BRAND}" "${RESET}"
    case "${UPDATE_RESULT}" in
        updated)
            printf '  %sVaultCenter is up to date%s\n\n' "${BOLD}" "${RESET}"
            printf '  Version:              %s -> %s\n' "${UPDATE_FROM}" "${UPDATE_TO}"
            printf '  Backup:               %s\n' "${UPDATE_BACKUP:-none}"
            ;;
        unchanged)
            printf '  %sVaultCenter is already at version %s%s\n\n' "${BOLD}" "${UPDATE_TO}" "${RESET}"
            printf '  Nothing has been changed.\n'
            ;;
        refused)
            printf '  %sUpdate refused%s: nothing has been changed.\n' "${BOLD}" "${RESET}"
            ;;
        rolled_back)
            printf '  %sUpdate cancelled%s: the appliance is back on the previous version (%s).\n\n' "${BOLD}" "${RESET}" "${UPDATE_FROM}"
            printf '  Backup:               %s\n' "${UPDATE_BACKUP:-none}"
            ;;
        rollback_failed)
            printf '  %sThe rollback failed%s: the state of the appliance must be checked.\n\n' "${BOLD}" "${RESET}"
            printf '  Failed step:          %s\n' "${UPDATE_STEP:-unknown}"
            printf '  Backup:               %s\n' "${UPDATE_BACKUP:-none}"
            printf '  Follow the manual procedure ("Rollback") in\n'
            printf '  %s with this backup.\n' "$(doc_path)"
            printf '  Nothing is retried automatically.\n'
            ;;
    esac
    # Only the VTLs the updater names: a run that entered none leaves none.
    if [[ "${#UPDATE_STILL[@]}" -gt 0 ]]; then
        case "${UPDATE_RESULT}" in
            rolled_back | rollback_failed)
                printf '\n  THESE VTLS ARE STILL IN MAINTENANCE. Taking them out of it is your decision:\n'
                printf '  do it in the console, once you have checked the appliance.\n'
                ;;
            *)
                printf '\n  These VTLs are still in maintenance: they have not been taken out of it.\n'
                printf '  Take them out of maintenance in the console.\n'
                ;;
        esac
        for line in "${UPDATE_STILL[@]}"; do
            printf '    - %s\n' "${line}"
        done
    fi
    # The reasons of every result: after a success they say what remains to do.
    if [[ "${#UPDATE_REASONS[@]}" -gt 0 ]]; then
        case "${UPDATE_RESULT}" in
            updated | unchanged) printf '\n  Next steps and notes:\n' ;;
            *) printf '\n  Reasons:\n' ;;
        esac
        for line in "${UPDATE_REASONS[@]}"; do
            printf '    - %s\n' "${line}"
        done
    fi
    if [[ "${#UPDATE_WARNINGS[@]}" -gt 0 ]]; then
        printf '\n  Warnings:\n'
        for line in "${UPDATE_WARNINGS[@]}"; do
            printf '    - %s\n' "${line}"
        done
    fi
    printf '\n  Log:                  /var/log/vaultcenter-install.log\n'
    printf '%s============================================================%s\n' "${BRAND}" "${RESET}"
}

step_summary() {
    if [[ "${MODE}" == "update" ]]; then
        # Never stopped half-way by a terminal that went away (SIGHUP).
        print_update_summary || true
        log "update summary: ${UPDATE_RESULT}, code ${UPDATE_EXIT}"
        if [[ -n "${DEFERRED_SIGNAL}" ]]; then
            log "signal ${DEFERRED_SIGNAL} received during the update: exiting with the code of the result (${UPDATE_EXIT})"
            printf 'Signal %s received during the update: the script waited for the result of the updater, given above; it exits with the code of that result (%s).\n' \
                "${DEFERRED_SIGNAL}" "${UPDATE_EXIT}" >&2 || true
        fi
        DEFER_SIGNALS=0
        if [[ "${UPDATE_EXIT}" -ne 0 ]]; then
            exit "${UPDATE_EXIT}"
        fi
        ok "update finished"
        return
    fi
    print_summary
    ADMIN_PASSWORD=""
    # Nothing may make a later run look like a resume: it is an update.
    if [[ -f "${STATE_FILE}" ]]; then
        sed -i -e '/^done=/d' -e '/^installing=1$/d' "${STATE_FILE}"
    fi
    ok "installation finished"
}

# ------------------------------------------------------------- cleanup
remove_payload_container() {
    if [[ -n "${PAYLOAD_CONTAINER}" ]]; then
        local name="${PAYLOAD_CONTAINER}"
        PAYLOAD_CONTAINER=""
        run_logged docker rm -f "${name}" || log "the temporary container ${name} could not be removed"
    fi
}

# registry_logout: the registry credential must not stay in the docker client
# configuration once the image is pulled.
registry_logout() {
    if [[ -n "${LOGIN_REGISTRY}" ]]; then
        local registry="${LOGIN_REGISTRY}"
        LOGIN_REGISTRY=""
        run_logged docker logout "${registry}" || log "docker logout ${registry} failed"
    fi
}

# cleanup: runs on every exit (success, fail, INT, TERM, HUP); keeps the exit code
# and never fails. Add further undo actions here, each idempotent.
cleanup() {
    remove_payload_container || true
    registry_logout || true
    if [[ -n "${UPDATER_OUTPUT}" ]]; then
        rm -f "${UPDATER_OUTPUT}" || true
    fi
}

on_exit() {
    local rc=$?
    trap - EXIT INT TERM HUP
    cleanup
    exit "${rc}"
}

# ----------------------------------------------------------------- main
main() {
    local index name recorded=""
    parse_options "$@"
    setup_rendering
    require_root
    init_log
    trap on_exit EXIT
    trap 'on_signal INT 130' INT
    trap 'on_signal TERM 143' TERM
    trap 'on_signal HUP 129' HUP
    log "options : version=${VERSION:-latest} yes=${ASSUME_YES} quiet=${QUIET} skip_preflight=${SKIP_PREFLIGHT} registry_user=${REGISTRY_USER:-} token=${TOKEN:+given} registry_token=${REGISTRY_TOKEN:+given}"
    choose_mode
    select_steps
    set_apt_proxy
    if [[ "${MODE}" == "resume" && -n "${VERSION}" ]]; then
        recorded="$(state_value version)"
        if [[ -n "${recorded}" && "${recorded}" != "${VERSION}" ]]; then
            resume_version_conflict "${recorded}"
        fi
    fi
    confirm
    banner
    # An install or a resume is under way until the summary removes this line.
    if [[ "${MODE}" != "update" ]] && ! grep -qxF 'installing=1' "${STATE_FILE}" 2>/dev/null; then
        record_state installing 1
    fi

    # state_value is empty in update mode: a leftover state file is ignored.
    IMAGE_REF="$(state_value image)"
    IMAGE_VERSION="$(state_value version)"

    for index in "${!STEP_NAMES[@]}"; do
        name="${STEP_NAMES[${index}]}"
        step "$((index + 1))" "${STEP_LABELS[${index}]}"
        if [[ "${ALWAYS_RERUN}" != *" ${name} "* ]] && step_is_done "${name}"; then
            ok "already done (resumed)"
            continue
        fi
        "step_${name}"
    done
}

main "$@"

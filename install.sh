#!/usr/bin/env bash
#
# voxtype installer — one-command setup for voice-to-text dictation on Linux.
#
#   curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash
#
# What it does:
#   1. Detects your display server (X11 / Wayland) and compositor
#   2. Installs runtime dependencies via your package manager
#   3. Installs the voxtype binary (GitHub release, falls back to source build)
#   4. Writes ~/.config/voxtype/config.toml (API key from $GROQ_API_KEY)
#   5. Sets up autostart for the background daemon
#   6. Binds Ctrl+Space as the dictation hotkey
#   7. Starts the daemon so you can use it immediately
#
# Safe by default: idempotent, never overwrites an existing config, and
# supports --dry-run to preview every action without touching the system.
#
# Usage:
#   bash install.sh [options]
#
# Options:
#   -h, --help           Show this help
#   -v, --verbose        Print extra detail
#       --dry-run        Preview actions without changing anything
#       --api-key KEY    Groq API key (default: $GROQ_API_KEY env var)
#       --method METHOD  "auto" (default), "binary" (download release), "source" (cargo build)
#       --prefix DIR     Install prefix (default: $HOME/.local)
#       --no-hotkey      Skip hotkey binding
#       --no-autostart   Skip autostart setup
#       --no-start       Do not start the daemon after installing
#       --branch NAME    Branch for source builds (default: main)
#
set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO="atheerium/voxtype"
REPO_URL="https://github.com/${REPO}"
API_URL="https://api.github.com/repos/${REPO}"

PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="${PREFIX}/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/voxtype"
CONFIG_FILE="${CONFIG_DIR}/config.toml"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
AUTOSTART_FILE="${AUTOSTART_DIR}/voxtype.desktop"

METHOD="auto"
DRY_RUN=0
VERBOSE=0
DO_HOTKEY=1
DO_AUTOSTART=1
DO_START=1
API_KEY="${GROQ_API_KEY:-}"
BRANCH="main"

# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

c_reset="\033[0m"; c_green="\033[32m"; c_yellow="\033[33m"; c_cyan="\033[36m"; c_red="\033[31m"
if [ ! -t 1 ]; then c_reset=""; c_green=""; c_yellow=""; c_cyan=""; c_red=""; fi

say()  { printf "${c_green}==>${c_reset} %s\n" "$*"; }
info() { printf "${c_cyan}    %s${c_reset}\n" "$*"; }
warn() { printf "${c_yellow}WARN${c_reset} %s\n" "$*"; }
die()  { printf "${c_red}ERROR${c_reset} %s\n" "$*" >&2; exit 1; }
dbg()  { [ "$VERBOSE" -eq 1 ] && printf "    [dbg] %s\n" "$*"; return 0; }

# Run a command, honoring dry-run mode.
run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf "${c_cyan}  dry-run:${c_reset} %s\n" "$*"
    return 0
  fi
  dbg "exec: $*"
  "$@"
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

usage() {
  cat <<'HELP'
voxtype installer — one-command setup for voice-to-text dictation on Linux.

  curl -fsSL https://github.com/atheerium/voxtype/releases/latest/download/install.sh | bash

Options:
  -h, --help           Show this help
  -v, --verbose        Print extra detail
      --dry-run        Preview actions without changing anything
      --api-key KEY    Groq API key (default: $GROQ_API_KEY env var)
      --method METHOD  "auto" (default), "binary" (download release), "source" (cargo build)
      --prefix DIR     Install prefix (default: $HOME/.local)
      --no-hotkey      Skip hotkey binding
      --no-autostart   Skip autostart setup
      --no-start       Do not start the daemon after installing
      --branch NAME    Branch for source builds (default: main)
HELP
}

while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    -v|--verbose) VERBOSE=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --api-key) shift; API_KEY="$1" ;;
    --method) shift; METHOD="$1" ;;
    --prefix) shift; PREFIX="$1"; BIN_DIR="${PREFIX}/bin" ;;
    --no-hotkey) DO_HOTKEY=0 ;;
    --no-autostart) DO_AUTOSTART=0 ;;
    --no-start) DO_START=0 ;;
    --branch) shift; BRANCH="$1" ;;
    *) die "Unknown option: $1 (run --help)" ;;
  esac
  shift
done

case "$METHOD" in
  auto|binary|source) ;;
  *) die "--method must be auto, binary, or source (got: $METHOD)" ;;
esac

# ---------------------------------------------------------------------------
# System detection
# ---------------------------------------------------------------------------

os="$(uname -s)"
arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) target_arch="x86_64" ;;
  aarch64|arm64) target_arch="aarch64" ;;
  *) target_arch="$arch" ;;
esac

# Display server: Wayland wins on XWayland, matching voxtype's own detection.
if [ -n "${WAYLAND_DISPLAY:-}" ] || [ "${XDG_SESSION_TYPE:-}" = "wayland" ]; then
  display_server="wayland"
elif [ -n "${DISPLAY:-}" ]; then
  display_server="x11"
else
  display_server="none"
fi

desktop="${XDG_CURRENT_DESKTOP:-}"
desktop_lc="$(printf '%s' "$desktop" | tr '[:upper:]' '[:lower:]')"
compositor="unknown"
case "$desktop_lc" in
  *sway*) compositor="sway" ;;
  *hyprland*) compositor="hyprland" ;;
  *kde*|*plasma*) compositor="kde" ;;
  *gnome*|*mutter*|*ubuntu*|*pantheon*|*cinnamon*|*budgie*) compositor="gnome" ;;
  *xfce*) compositor="xfce" ;;
  *i3*) compositor="i3" ;;
  *mate*) compositor="mate" ;;
esac

# ---------------------------------------------------------------------------
# Package manager detection
# ---------------------------------------------------------------------------

pkg_mgr=""
for pm in apt-get dnf pacman zypper apk emerge; do
  if command -v "$pm" >/dev/null 2>&1; then pkg_mgr="$pm"; break; fi
done

pkg_install() { # $@ = package names
  local pkgs=("$@")
  [ "${#pkgs[@]}" -eq 0 ] && return 0
  local sudo_cmd=()
  [ "$(id -u)" -ne 0 ] && sudo_cmd=(sudo)
  case "$pkg_mgr" in
    apt-get) run "${sudo_cmd[@]}" apt-get install -y "${pkgs[@]}" ;;
    dnf)     run "${sudo_cmd[@]}" dnf install -y "${pkgs[@]}" ;;
    pacman)  run "${sudo_cmd[@]}" pacman -S --noconfirm "${pkgs[@]}" ;;
    zypper)  run "${sudo_cmd[@]}" zypper --non-interactive install "${pkgs[@]}" ;;
    apk)     run "${sudo_cmd[@]}" apk add "${pkgs[@]}" ;;
    emerge)  run "${sudo_cmd[@]}" emerge --ask=n "${pkgs[@]}" ;;
    *) return 1 ;;
  esac
}

deps_for_env() {
  local deps=()
  case "$display_server" in
    wayland) deps+=(wl-clipboard wtype) ;;
    x11)     deps+=(xdotool xsel xclip) ;;
  esac
  # Optional but recommended
  command -v notify-send >/dev/null 2>&1 || deps+=(notify-send-pkg)
  command -v pactl >/dev/null 2>&1 || deps+=(pactl-pkg)
  printf '%s\n' "${deps[@]}"
}

# The command that proves a logical dependency is already installed
# (package names and binaries don't always match, e.g. wl-clipboard -> wl-copy).
detect_cmd_for() {
  case "$1" in
    wl-clipboard) echo wl-copy ;;
    notify-send-pkg) echo notify-send ;;
    pactl-pkg) echo pactl ;;
    *) echo "$1" ;;
  esac
}

pkg_for() {
  case "$pkg_mgr:$1" in
    apt-get:ffmpeg) echo ffmpeg ;;
    dnf:ffmpeg) echo ffmpeg ;;
    pacman:ffmpeg) echo ffmpeg ;;
    zypper:ffmpeg) echo ffmpeg ;;
    apk:ffmpeg) echo ffmpeg ;;
    emerge:ffmpeg) echo media-video/ffmpeg ;;
    apt-get:xdotool) echo xdotool ;;
    dnf:xdotool) echo xdotool ;;
    pacman:xdotool) echo xdotool ;;
    zypper:xdotool) echo xdotool ;;
    apt-get:xsel) echo xsel ;;
    dnf:xsel) echo xsel ;;
    pacman:xsel) echo xsel ;;
    zypper:xsel) echo xsel ;;
    apt-get:xclip) echo xclip ;;
    dnf:xclip) echo xclip ;;
    pacman:xclip) echo xclip ;;
    zypper:xclip) echo xclip ;;
    apt-get:wl-clipboard) echo wl-clipboard ;;
    dnf:wl-clipboard) echo wl-clipboard ;;
    pacman:wl-clipboard) echo wl-clipboard ;;
    zypper:wl-clipboard) echo wl-clipboard ;;
    apk:wl-clipboard) echo wl-clipboard ;;
    apt-get:wtype) echo wtype ;;
    dnf:wtype) echo wtype ;;
    pacman:wtype) echo wtype ;;
    zypper:wtype) echo wtype ;;
    apt-get:notify-send-pkg) echo libnotify-bin ;;
    dnf:notify-send-pkg) echo libnotify ;;
    pacman:notify-send-pkg) echo libnotify ;;
    zypper:notify-send-pkg) echo libnotify4 ;;
    apk:notify-send-pkg) echo libnotify ;;
    apt-get:pactl-pkg) echo pulseaudio-utils ;;
    dnf:pactl-pkg) echo pulseaudio-utils ;;
    pacman:pactl-pkg) echo libpulse ;;
    zypper:pactl-pkg) echo pulseaudio-utils ;;
    apk:pactl-pkg) echo pulseaudio-utils ;;
    *) echo "$1" ;;
  esac
}

# ---------------------------------------------------------------------------
# Binary installation
# ---------------------------------------------------------------------------

have_cmd() { command -v "$1" >/dev/null 2>&1; }

install_binary_release() {
  local tag asset url tmp
  info "Looking up latest release from ${REPO_URL}..."
  if ! have_cmd curl; then
    warn "curl is not available; falling back to source build."
    return 1
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    info "Would download the latest release from ${REPO_URL}/releases"
    return 0
  fi
  tag="$(curl -fsSL "${API_URL}/releases/latest" 2>/dev/null | (command -v jq >/dev/null 2>&1 && jq -r '.tag_name' || sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p') | head -n1 || true)"
  if [ -z "$tag" ] || [ "$tag" = "null" ]; then
    warn "No published release yet; falling back to source build."
    return 1
  fi
  for triple in "${target_arch}-unknown-linux-musl" "${target_arch}-unknown-linux-gnu"; do
    asset="voxtype-${triple}.tar.gz"
    url="${REPO_URL}/releases/download/${tag}/${asset}"
    info "Trying ${asset} ..."
    if curl -fsSL -o /tmp/voxtype-install.tar.gz "$url" 2>/dev/null; then
      # Verify checksum when available
      local sum_file="/tmp/voxtype-install.sha256"
      if curl -fsSL -o "$sum_file" "${REPO_URL}/releases/download/${tag}/sha256sums.txt" 2>/dev/null \
         && command -v sha256sum >/dev/null 2>&1 \
         && grep -q "${asset}" "$sum_file"; then
        local want got
        want="$(grep "${asset}" "$sum_file" | awk '{print $1}')"
        got="$(sha256sum /tmp/voxtype-install.tar.gz | awk '{print $1}')"
        if [ "$want" != "$got" ]; then
          warn "Checksum mismatch for ${asset}; skipping this artifact."
          continue
        fi
      fi
      tmp="$(mktemp -d)"
      if tar -xzf /tmp/voxtype-install.tar.gz -C "$tmp" && [ -f "$tmp/voxtype" ]; then
        mkdir -p "$BIN_DIR"
        run cp "$tmp/voxtype" "${BIN_DIR}/voxtype"
        run chmod +x "${BIN_DIR}/voxtype"
        rm -rf "$tmp" /tmp/voxtype-install.tar.gz /tmp/voxtype-install.sha256
        return 0
      fi
      rm -rf "$tmp"
      warn "Unexpected release archive layout for ${asset}; trying next."
    fi
  done
  warn "Could not download a prebuilt binary; falling back to source build."
  return 1
}

install_binary_source() {
  if [ "$DRY_RUN" -eq 1 ]; then
    info "Would build from source (cargo install --git ${REPO_URL})"
    return 0
  fi
  if have_cmd cargo; then
    say "Building voxtype from source with cargo (this takes a few minutes)..."
    if ! run cargo install --git "${REPO_URL}" --branch "$BRANCH" --root "$PREFIX" --locked; then
      # rustup shims exist but no default toolchain is configured; set one
      # and retry once. This is safe and exactly what a Rust user wants.
      if command -v rustup >/dev/null 2>&1 && ! rustup show active-toolchain >/dev/null 2>&1; then
        warn "No default Rust toolchain configured; installing stable via rustup..."
        run rustup default stable
        run cargo install --git "${REPO_URL}" --branch "$BRANCH" --root "$PREFIX" --locked
      else
        return 1
      fi
    fi
    return 0
  fi
  if have_cmd git && have_cmd curl; then
    info "Installing Rust toolchain (rustup) to build from source..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1 \
      || die "Failed to install rustup. Install Rust manually: https://rustup.rs"
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
    run cargo install --git "${REPO_URL}" --branch "$BRANCH" --root "$PREFIX" --locked
    return 0
  fi
  die "Cannot build from source: neither cargo nor (git+curl) is available. Install Rust: https://rustup.rs"
}

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

write_config() {
  if [ -f "$CONFIG_FILE" ]; then
    info "Config already exists at ${CONFIG_FILE} (leaving untouched)."
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    info "Would write ${CONFIG_FILE}"
    return 0
  fi
  mkdir -p "$CONFIG_DIR"
  {
    echo "# voxtype configuration"
    if [ -n "$API_KEY" ]; then
      echo "groq_api_key = \"${API_KEY}\""
    else
      echo "# groq_api_key = \"gsk_...\"  # or export GROQ_API_KEY"
    fi
    echo "language = \"en\"            # ISO-639-1 code, optional"
  } > "$CONFIG_FILE"
  if [ -z "$API_KEY" ]; then
    warn "No GROQ_API_KEY found. voxtype will use the env var at runtime, or"
    warn "edit ${CONFIG_FILE} and add groq_api_key = \"gsk_...\" (console.groq.com)."
  fi
}

# ---------------------------------------------------------------------------
# Autostart + hotkey
# ---------------------------------------------------------------------------

setup_autostart() {
  if [ "$DRY_RUN" -eq 1 ]; then
    info "Would write ${AUTOSTART_FILE}"
    return 0
  fi
  mkdir -p "$AUTOSTART_DIR"
  {
    echo "[Desktop Entry]"
    echo "Type=Application"
    echo "Name=voxtype"
    echo "Comment=Voice-to-text dictation daemon"
    echo "Exec=${BIN_DIR}/voxtype --daemon"
    echo "X-GNOME-Autostart-enabled=true"
  } > "$AUTOSTART_FILE"
  info "Autostart entry written to ${AUTOSTART_FILE}"
}

append_line_if_missing() { # $1 = file, $2 = line
  if [ ! -f "$1" ]; then
    if [ "$DRY_RUN" -eq 1 ]; then
      info "Would create $1 and add: $2"
      return 0
    fi
    mkdir -p "$(dirname "$1")"
    touch "$1"
  fi
  if ! grep -qF -- "$2" "$1" 2>/dev/null; then
    run bash -c 'printf "%s\n" "$1" >> "$2"' bash "$2" "$1"
    return 0
  fi
  info "Already configured: $2"
  return 0
}

setup_hotkey() {
  local bin="$BIN_DIR/voxtype"
  case "$compositor" in
    sway)
      local cfg="$HOME/.config/sway/config"
      say "Binding Ctrl+Space in Sway (${cfg})"
      append_line_if_missing "$cfg" "exec_always ${bin} --daemon"
      append_line_if_missing "$cfg" "bindsym Ctrl+space exec ${bin}"
      if [ "$DRY_RUN" -eq 0 ] && command -v swaymsg >/dev/null 2>&1; then
        swaymsg reload >/dev/null 2>&1 || true
      fi
      ;;
    hyprland)
      local cfg="$HOME/.config/hypr/hyprland.conf"
      say "Binding Ctrl+Space in Hyprland (${cfg})"
      append_line_if_missing "$cfg" "exec-once = ${bin} --daemon"
      append_line_if_missing "$cfg" "bind = CTRL, SPACE, exec, ${bin}"
      ;;
    gnome)
      warn "GNOME: no universal hotkey is possible without an extension."
      warn "Add a custom shortcut in Settings -> Keyboard -> Keyboard Shortcuts:"
      info "  Name: voxtype   Command: ${bin}"
      ;;
    xfce)
      say "Binding Ctrl+Space in XFCE via xfconf-query"
      if command -v xfconf-query >/dev/null 2>&1; then
        run xfconf-query -c xfce4-keyboard-shortcuts -p "/commands/custom/<Primary>space" -s "$bin" --create -t string
      else
        warn "xfconf-query not found; add a custom shortcut for ${bin} manually."
      fi
      ;;
    kde)
      warn "KDE Plasma: add a custom shortcut for ${bin} in System Settings -> Shortcuts."
      ;;
    *)
      warn "No automatic hotkey for '${compositor:-unknown}'. Add a custom shortcut for ${bin} manually."
      ;;
  esac
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
  say "voxtype installer"
  echo
  info "System: ${os} ${arch} | Display: ${display_server} | Desktop: ${desktop:-none} (${compositor})"
  info "Install prefix: ${BIN_DIR}"
  [ "$DRY_RUN" -eq 1 ] && warn "DRY RUN: nothing below will actually be executed."
  echo

  [ "$os" = "Linux" ] || warn "voxtype is designed for Linux; continuing anyway."

  # 1. Dependencies
  if [ -n "$pkg_mgr" ]; then
    local deps=()
    command -v ffmpeg >/dev/null 2>&1 || deps+=(ffmpeg)
    local need detect
    while read -r need; do
      [ -n "$need" ] || continue
      detect="$(detect_cmd_for "$need")"
      command -v "$detect" >/dev/null 2>&1 || deps+=("$(pkg_for "$need")")
    done < <(deps_for_env)
    if [ "${#deps[@]}" -gt 0 ]; then
      say "Installing dependencies via ${pkg_mgr}: ${deps[*]}"
      pkg_install "${deps[@]}" || warn "Dependency installation failed; voxtype will report missing tools at runtime."
    else
      say "All runtime dependencies already present."
    fi
  else
    warn "No supported package manager found; install ffmpeg and clipboard tools manually."
  fi

  # 2. Binary
  if [ -x "${BIN_DIR}/voxtype" ] && [ "$METHOD" = "auto" ]; then
    say "voxtype already installed at ${BIN_DIR}/voxtype."
  else
    say "Installing voxtype..."
    local ok=1
    if [ "$METHOD" != "source" ]; then
      install_binary_release || ok=0
    else
      ok=0
    fi
    if [ "$ok" -eq 0 ]; then
      install_binary_source
    fi
    if [ "$DRY_RUN" -eq 1 ]; then
      info "Would install voxtype to ${BIN_DIR}/voxtype"
    else
      say "Installed to ${BIN_DIR}/voxtype"
    fi
  fi

  # 3. Config
  say "Writing configuration"
  write_config

  # 4. Autostart
  if [ "$DO_AUTOSTART" -eq 1 ]; then
    say "Setting up autostart"
    setup_autostart
  fi

  # 5. Hotkey
  if [ "$DO_HOTKEY" -eq 1 ]; then
    say "Binding hotkey (Ctrl+Space)"
    setup_hotkey
  fi

  # 6. Start daemon
  if [ "$DO_START" -eq 1 ] && [ "$DRY_RUN" -eq 0 ]; then
    say "Starting voxtype daemon"
    nohup "${BIN_DIR}/voxtype" --daemon >/dev/null 2>&1 || warn "Could not start daemon (missing display server?)."
  fi

  echo
  say "Done! voxtype is installed."
  echo
  info "  Press  Ctrl+Space  to start recording, speak, press Ctrl+Space again."
  info "  Transcribed text is pasted into your focused app automatically."
  info "  Text is always on the clipboard too (manual paste: Ctrl+V / Ctrl+Shift+V)."
  info "  Logs: ${XDG_DATA_HOME:-$HOME/.local/share}/voxtype/daemon.log"
  echo
  if [ "$DRY_RUN" -eq 0 ] && [ -n "$API_KEY" ]; then
    info "Groq API key detected. You're all set."
  elif [ "$DRY_RUN" -eq 1 ]; then
    info "Run without --dry-run to actually install."
  else
    info "Set GROQ_API_KEY (or edit ${CONFIG_FILE}) before first transcription."
  fi
}

main "$@"

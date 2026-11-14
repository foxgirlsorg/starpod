#!/usr/bin/env bash
# Headless screenshots of the Starpod UI, for checking UI changes by eye.
# Runs INSIDE the dev image (Xvfb, xdotool and ImageMagick are installed there).
#
# One-liner (rebuild the image first if the Dockerfile changed):
#   docker compose build dev
#   docker compose run --rm dev bash tools/ui-shot.sh [--empty] [page]
# Same thing via the shot service:
#   docker compose run --rm shot [--empty] [page]
#
# [page] is one of: home profiles proxies rules logs settings connections. Omit it for all seven.
# By default STARPOD_UI_DEMO=1 fills the window with the demo data (ui-spec.md
# section 7). --empty, as the first argument, leaves it unset to shoot empty states.
# Output: /work/shots/<page>-<width>x<height>.png, i.e. <repo>/shots/ on the host.
# STARPOD_UI_LANG=ru (e.g. docker compose run --rm -e STARPOD_UI_LANG=ru shot home) shoots
# the Russian UI into <page>-ru-<width>x<height>.png. Unset or en shoots English.
# With --empty: /work/shots/<page>-empty-<width>x<height>.png.
# Sizes: 1100x720 and 900x600. Each shot gets its own Xvfb and a fresh temporary
# HOME (with XDG dirs under it), so the user's real data is never touched.
# Exit status is 1 if any shot failed; the other shots are still taken.
set -euo pipefail

ALL_PAGES=(home profiles proxies rules logs settings connections)   # index = Bridge.active-page
SIZES=("1100x720" "900x600")
DISPLAY_NUM=99
BIN=/target/debug/starpod
OUT=/work/shots

DEMO=1
if [ "${1:-}" = "--empty" ]; then
    DEMO=0
    shift
fi

if [ "$#" -gt 1 ]; then
    echo "usage: tools/ui-shot.sh [--empty] [home|profiles|proxies|rules|logs|settings|connections]" >&2
    exit 2
fi

PAGES=("${ALL_PAGES[@]}")
if [ "$#" -eq 1 ]; then
    PAGES=("$1")
    known=0
    for name in "${ALL_PAGES[@]}"; do
        [ "$name" = "$1" ] && known=1
    done
    if [ "$known" -eq 0 ]; then
        echo "unknown page: $1 (use one of: ${ALL_PAGES[*]})" >&2
        exit 2
    fi
fi

cd /work
echo "building starpod-app (debug)"
cargo build -p starpod-app

mkdir -p "$OUT"

XVFB_PID=""
APP_PID=""
TMP_HOME=""

# Stop the app (its whole process group, it runs under setsid), then Xvfb.
stop_all() {
    if [ -n "$APP_PID" ]; then
        kill -TERM -- "-$APP_PID" 2>/dev/null || true
        wait "$APP_PID" 2>/dev/null || true
        APP_PID=""
    fi
    if [ -n "$XVFB_PID" ]; then
        kill -TERM "$XVFB_PID" 2>/dev/null || true
        wait "$XVFB_PID" 2>/dev/null || true
        XVFB_PID=""
    fi
    if [ -n "$TMP_HOME" ]; then
        rm -rf "$TMP_HOME"
        TMP_HOME=""
    fi
}
trap stop_all EXIT

# Take one screenshot. Arguments: page name, page index, width, height.
shoot() {
    local page="$1" index="$2" w="$3" h="$4"
    local display=":$DISPLAY_NUM"
    local tag=""
    [ "$DEMO" -eq 1 ] || tag="-empty"
    # STARPOD_UI_LANG=ru (passed through from the caller's environment) shoots the
    # Russian UI; those files get a -ru suffix so both languages can sit side by side.
    local lang_tag=""
    case "${STARPOD_UI_LANG:-}" in
        ru|RU) lang_tag="-ru" ;;
    esac
    local file="$OUT/$page$tag$lang_tag-${w}x${h}.png"
    # The demo flag is set or removed explicitly, so a value in the caller's
    # environment cannot leak into an --empty shot.
    local demo_arg=(-u STARPOD_UI_DEMO)
    [ "$DEMO" -eq 1 ] && demo_arg=(STARPOD_UI_DEMO=1)

    TMP_HOME=$(mktemp -d)
    mkdir -p "$TMP_HOME/home"
    rm -f "$file"

    # A virtual screen of exactly the window size, so the window fills it.
    Xvfb "$display" -screen 0 "${w}x${h}x24" -nolisten tcp >"$TMP_HOME/xvfb.log" 2>&1 &
    XVFB_PID=$!
    local ready=0
    for _ in $(seq 1 50); do
        if DISPLAY="$display" xdotool getdisplaygeometry >/dev/null 2>&1; then
            ready=1
            break
        fi
        sleep 0.2
    done
    if [ "$ready" -eq 0 ]; then
        echo "FAIL $page ${w}x${h}: Xvfb did not start (see $TMP_HOME/xvfb.log)" >&2
        return 1
    fi

    # `env` execs `setsid`, which execs the app, so $! is the app's own pid and
    # it leads its own process group.
    env "${demo_arg[@]}" DISPLAY="$display" \
        SLINT_BACKEND=winit-software \
        STARPOD_UI_PAGE="$index" \
        HOME="$TMP_HOME/home" \
        XDG_CONFIG_HOME="$TMP_HOME/config" \
        XDG_DATA_HOME="$TMP_HOME/data" \
        XDG_CACHE_HOME="$TMP_HOME/cache" \
        XDG_STATE_HOME="$TMP_HOME/state" \
        setsid "$BIN" >"$TMP_HOME/app.log" 2>&1 &
    APP_PID=$!

    # Wait for the window, up to 20 s. The tray fails on Xvfb (no
    # StatusNotifier host); the app carries on without it.
    local wid=""
    for _ in $(seq 1 100); do
        wid=$(DISPLAY="$display" xdotool search --name Starpod 2>/dev/null | head -n1 || true)
        [ -n "$wid" ] && break
        if ! kill -0 "$APP_PID" 2>/dev/null; then
            break
        fi
        sleep 0.2
    done
    if [ -z "$wid" ]; then
        echo "FAIL $page ${w}x${h}: no Starpod window appeared. App log tail:" >&2
        tail -n 20 "$TMP_HOME/app.log" >&2 || true
        return 1
    fi

    DISPLAY="$display" xdotool windowmove --sync "$wid" 0 0 || true
    DISPLAY="$display" xdotool windowsize --sync "$wid" "$w" "$h" || true
    sleep 2

    if ! DISPLAY="$display" import -window root "$file"; then
        echo "FAIL $page ${w}x${h}: capture failed" >&2
        return 1
    fi
    echo "wrote $file"
    stop_all
}

failures=0
for page in "${PAGES[@]}"; do
    index=0
    for i in "${!ALL_PAGES[@]}"; do
        [ "${ALL_PAGES[$i]}" = "$page" ] && index="$i"
    done
    for size in "${SIZES[@]}"; do
        w="${size%x*}"
        h="${size#*x}"
        if ! shoot "$page" "$index" "$w" "$h"; then
            failures=$((failures + 1))
            stop_all
        fi
    done
done

if [ "$failures" -gt 0 ]; then
    echo "$failures screenshot(s) failed" >&2
    exit 1
fi
echo "done: screenshots are in $OUT"

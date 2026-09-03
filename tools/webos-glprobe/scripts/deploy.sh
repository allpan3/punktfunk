#!/bin/sh
# Put the probe on a dev-mode webOS TV and run it.
#
#   TV_HOST=192.168.1.x ./scripts/deploy.sh deploy
#   TV_HOST=192.168.1.x ./scripts/deploy.sh run
#   TV_HOST=192.168.1.x ./scripts/deploy.sh log
#   TV_HOST=192.168.1.x ./scripts/deploy.sh restore
#
# Why it swaps the installed app's binary rather than installing a second app: webOS only
# composites for the SAM-managed foreground app, and dev mode grants the Luna `public` group BY
# EXECUTABLE PATH — a probe anywhere else gets a black screen. The original binary is backed up
# to `punktfunk-webos.orig` on first deploy; `restore` puts it back. Nothing here is one-way.
#
# Env: TV_HOST (required), TV_PORT (9922), TV_USER (prisoner), TV_KEY (~/.ssh/tv_webos_nopass),
#      APP (io.dyptan.punktfunk.webos).
set -eu

# No apostrophe in this message: a quote inside a ${var:?word} expansion desynchronises bash's
# parser, and the script then dies with a syntax error dozens of lines further down.
: "${TV_HOST:?set TV_HOST to the address of the TV}"
TV_PORT=${TV_PORT:-9922}
TV_USER=${TV_USER:-prisoner}
TV_KEY=${TV_KEY:-$HOME/.ssh/tv_webos_nopass}
APP=${APP:-io.dyptan.punktfunk.webos}
DIR=/media/developer/apps/usr/palm/applications/$APP

HERE=$(cd "$(dirname "$0")" && pwd)
OUT=${OUT:-$(dirname "$HERE")/out}

# webOS's dropbear predates the modern defaults, hence the two algorithm opt-ins. There is no
# pty in the app jail, so nothing interactive works over this.
SSH_OPTS="-o ConnectTimeout=10 -o StrictHostKeyChecking=no -o HostKeyAlgorithms=+ssh-rsa -o PubkeyAcceptedAlgorithms=+ssh-rsa -i $TV_KEY"
tv() { ssh $SSH_OPTS -p "$TV_PORT" "$TV_USER@$TV_HOST" "$@"; }
tvcp() { scp $SSH_OPTS -P "$TV_PORT" "$1" "$TV_USER@$TV_HOST:$2"; }

case "${1:-deploy}" in
deploy)
    [ -f "$OUT/pf-webos-glprobe" ] || { echo "no $OUT/pf-webos-glprobe — run scripts/build.sh first" >&2; exit 1; }
    echo "### backing up the original app binary (first deploy only)"
    tv "[ -f $DIR/bin/punktfunk-webos.orig ] || cp -a $DIR/bin/punktfunk-webos $DIR/bin/punktfunk-webos.orig" || true
    echo "### bundling libstdc++ 6.0.30 (the TV ships 6.0.29)"
    tvcp "$OUT/libstdc++.so.6" "$DIR/lib/libstdc++.so.6"
    echo "### staging the probe"
    tvcp "$OUT/pf-webos-glprobe" /tmp/glprobe
    # rm-then-cp: the file is root-owned 755, the directory is group-writable.
    tv "rm -f $DIR/bin/punktfunk-webos && cp /tmp/glprobe $DIR/bin/punktfunk-webos && chmod 755 $DIR/bin/punktfunk-webos && ls -l $DIR/bin $DIR/lib"
    ;;
run)
    # 🛑 `launch` on an ALREADY-RUNNING app only foregrounds it, so the swapped binary would
    # never start. Close first. luna-send's closeByAppId does nothing over ssh (no pty);
    # ares-launch is what actually works. From webosbrew/ares-cli-rs.
    command -v ares-launch >/dev/null || { echo "need ares-launch (cargo install --git https://github.com/webosbrew/ares-cli-rs ares-launch)" >&2; exit 1; }
    ares-launch -d "${ARES_DEVICE:-tv}" --close "$APP" 2>/dev/null || true
    sleep 2
    tv "rm -f $DIR/glprobe.log"
    ares-launch -d "${ARES_DEVICE:-tv}" "$APP"
    ;;
log)
    tv "cat $DIR/glprobe.log 2>/dev/null || echo '(no log yet)'"
    ;;
shot)
    # The probe reads its own framebuffer back at frame 90 — a picture of what the panel drew.
    scp $SSH_OPTS -P "$TV_PORT" "$TV_USER@$TV_HOST:$DIR/glprobe.png" "$OUT/glprobe.png"
    echo "$OUT/glprobe.png"
    ;;
stop)
    ares-launch -d "${ARES_DEVICE:-tv}" --close "$APP"
    ;;
restore)
    echo "### putting the app's own binary back and removing the bundled libstdc++"
    ares-launch -d "${ARES_DEVICE:-tv}" --close "$APP" 2>/dev/null || true
    sleep 1
    tv "[ -f $DIR/bin/punktfunk-webos.orig ] && rm -f $DIR/bin/punktfunk-webos && cp -a $DIR/bin/punktfunk-webos.orig $DIR/bin/punktfunk-webos && rm -f $DIR/bin/punktfunk-webos.orig; rm -f $DIR/lib/libstdc++.so.6 $DIR/glprobe.log $DIR/glprobe.png /tmp/glprobe; ls -l $DIR/bin $DIR/lib"
    ;;
*)
    echo "usage: TV_HOST=<addr> $0 {deploy|run|log|shot|stop|restore}" >&2
    exit 2
    ;;
esac

#!/bin/sh

set -eu

for command in xfconf-query xfce4-panel xdotool; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'Missing command: %s\n' "$command" >&2
    printf 'On Debian/Ubuntu, install: xfce4-genmon-plugin xdotool\n' >&2
    exit 1
  fi
done

if ! find /usr/lib -path '*/xfce4/panel/plugins/libgenmon.so' -print -quit 2>/dev/null |
  grep -q .; then
  printf 'Missing XFCE GenMon plugin.\n' >&2
  printf 'On Debian/Ubuntu, install: xfce4-genmon-plugin\n' >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
bin_dir="$HOME/.local/bin"
panel_dir="$HOME/.config/xfce4/panel"
autostart_dir="$HOME/.config/autostart"
mkdir -p "$bin_dir" "$panel_dir" "$autostart_dir"
install -m 0755 "$script_dir/nekoapi-widget-panel-status" "$bin_dir/"
install -m 0755 "$script_dir/nekoapi-widget-panel-toggle" "$bin_dir/"

plugin_id=$(find "$panel_dir" -maxdepth 1 -name 'genmon-*.rc' -type f -exec grep -l \
  'nekoapi-widget-panel-status' {} + 2>/dev/null |
  sed -n '1s/.*genmon-\([0-9][0-9]*\)\.rc/\1/p')

if [ -z "$plugin_id" ]; then
  plugin_id=$(xfconf-query -c xfce4-panel -lv |
    sed -n 's#^/plugins/plugin-\([0-9][0-9]*\).*#\1#p' |
    sort -n | tail -1)
  plugin_id=$((${plugin_id:-0} + 1))
  xfconf-query -c xfce4-panel -p "/plugins/plugin-$plugin_id" \
    -n -t string -s genmon

  systray_id=$(xfconf-query -c xfce4-panel -lv |
    sed -n 's#^/plugins/plugin-\([0-9][0-9]*\)[[:space:]]*systray$#\1#p' |
    head -1)
  panel_ids=$(LC_ALL=C xfconf-query -c xfce4-panel \
    -p /panels/panel-1/plugin-ids | sed -n '/^[0-9][0-9]*$/p')
  set -- xfconf-query -c xfce4-panel -p /panels/panel-1/plugin-ids
  inserted=0
  for id in $panel_ids; do
    set -- "$@" -t int -s "$id"
    if [ -n "$systray_id" ] && [ "$id" = "$systray_id" ]; then
      set -- "$@" -t int -s "$plugin_id"
      inserted=1
    fi
  done
  if [ "$inserted" = "0" ]; then
    set -- "$@" -t int -s "$plugin_id"
  fi
  "$@"
fi

cat >"$autostart_dir/nekoapi-widget.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=NekoAPI Widget
Comment=NekoAPI desktop usage monitor
Exec=env NEKOAPI_PANEL_BRIDGE=1 /usr/bin/nekoapi-widget
Icon=nekoapi-widget
Terminal=false
X-GNOME-Autostart-enabled=true
StartupNotify=false
OnlyShowIn=XFCE;
EOF

xfce4-panel --quit 2>/dev/null || true
sleep 2
cat >"$panel_dir/genmon-$plugin_id.rc" <<EOF
Command=$bin_dir/nekoapi-widget-panel-status
UseLabel=0
Text=
UpdatePeriod=5000
Font=Nimbus Sans 10
EOF

nohup xfce4-panel >/dev/null 2>&1 &
pkill -x nekoapi-widget 2>/dev/null || true
sleep 1
nohup env NEKOAPI_PANEL_BRIDGE=1 /usr/bin/nekoapi-widget >/dev/null 2>&1 &

printf 'NekoAPI Widget XFCE panel integration installed (plugin %s).\n' "$plugin_id"

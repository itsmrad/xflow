#!/bin/sh
# On package removal (deb "remove", rpm "0"; not upgrades), stop xflow.service in each
# running user manager. Enablement is per user and stays theirs; nothing is enabled system-wide.
case ${1:-} in remove|0) ;; *) exit 0 ;; esac
command -v loginctl >/dev/null 2>&1 || exit 0
loginctl list-users --no-legend 2>/dev/null | while read -r _ user _; do
    systemctl --user -M "$user@" stop xflow.service >/dev/null 2>&1 || true
done
exit 0

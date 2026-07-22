#!/bin/sh
# Activate the udev rule this package installed, so the scope is reachable without root.
#
# The rule itself lands in /usr/lib/udev/rules.d/70-mso5202d.rules; udev only picks it up
# after a reload, and only applies it to devices plugged in afterwards — hence the trigger,
# which re-applies it to a scope that is already connected.
#
# Never fail the installation over this: package installs happen in chroots, containers and
# image builders where udev is not running at all, and the rule is still correctly placed
# for the next boot.
set -e

if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules >/dev/null 2>&1 || true
    udevadm trigger --subsystem-match=usb >/dev/null 2>&1 || true
fi

exit 0

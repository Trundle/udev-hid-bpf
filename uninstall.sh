#!/bin/bash

SCRIPT_DIR=$(dirname "$(realpath "${BASH_SOURCE[0]}")")

usage () {
  echo "Usage: $(basename "$0") [-v|--verbose] [--dry-run] [--udevdir /etc/] [PREFIX]"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --help)
      usage
      exit 0
      ;;
    --verbose|-v)
      set -x
      shift
      ;;
    --dry-run)
      DRY_RUN="echo"
      shift
      ;;
    --udevdir)
      UDEVDIR=$2
      shift 2
      ;;
    --*)
      usage
      exit 1
      ;;
    *)
      break;
      ;;
  esac
done

if [ $# -gt 1 ]; then
  usage
  exit 1
fi

if [ "$EUID" -ne 0 ]
then
  echo "This script needs to update global udev rules, so please run as root."
  exit 1
fi

set -e

PREFIX=${1:-/usr/local}
if [ -z "$UDEVDIR" ]; then
  case ${PREFIX%/} in
    /usr)
      UDEVDIR="/usr/lib"
      ;;
    *)
      UDEVDIR="/etc"
      ;;
  esac
fi

BPF=$(ls "$SCRIPT_DIR"/lib/firmware/hid/bpf/*.bpf.o)
INSTALLED_BPF=${BPF//$SCRIPT_DIR/}
$DRY_RUN rm -f $INSTALLED_BPF
$DRY_RUN rm -f "$PREFIX"/bin/udev-hid-bpf
$DRY_RUN rm -f "$UDEVDIR"/udev/rules.d/99-hid-bpf.rules
$DRY_RUN rm -f "$UDEVDIR"/udev/hwdb.d/99-hid-bpf.hwdb
$DRY_RUN udevadm control --reload
$DRY_RUN systemd-hwdb update

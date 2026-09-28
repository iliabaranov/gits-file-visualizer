#!/usr/bin/env bash
# Measures system CPU, integrated-GPU busy time and per-process CPU over N seconds.
#   tools/measure.sh [SECONDS] [PID...]
# GPU busy = time the Intel GPU was not in its RC6 idle state (an upper bound).
set -euo pipefail
secs=${1:-20}; shift || true
rc6=$(ls /sys/class/drm/card*/gt/gt0/rc6_residency_ms 2>/dev/null | head -1)
cpu() { awk '/^cpu /{print $2+$3+$4+$6+$7+$8, $2+$3+$4+$5+$6+$7+$8}' /proc/stat; }
ptime() { awk '{print $14+$15}' "/proc/$1/stat" 2>/dev/null || echo 0; }
read -r b0 t0 < <(cpu); g0=${rc6:+$(cat "$rc6")}; w0=$(date +%s%3N)
declare -A p0; for p in "$@"; do p0[$p]=$(ptime "$p"); done
sleep "$secs"
read -r b1 t1 < <(cpu); g1=${rc6:+$(cat "$rc6")}; w1=$(date +%s%3N)
ncpu=$(nproc)
printf "system CPU busy: %5.1f%% of all %d cores\n" "$(echo "100*($b1-$b0)/($t1-$t0)" | bc -l)" "$ncpu"
[[ -n "$rc6" ]] && printf "iGPU busy:       %5.1f%%\n" "$(echo "100*(1-($g1-$g0)/($w1-$w0))" | bc -l)"
hz=$(getconf CLK_TCK)
for p in "$@"; do
  printf "  pid %-8s %5.1f%% of one core  %s\n" "$p" "$(echo "100*($(ptime "$p")-${p0[$p]})/$hz/$secs" | bc -l)" "$(tr '\0' ' ' </proc/$p/cmdline 2>/dev/null | cut -c1-60)"
done

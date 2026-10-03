#!/usr/bin/env bash
# Испытательный стенд для пробивки NAT (Linux, нужен root).
#
#   хост   10.1.0.2 ─┐
#   друг-1 10.1.0.3 ─┴ роутер1 (NAT) ──┐
#                                       ├── «интернет» 203.0.113.0/24: STUN .100/.101, ящик .100:8080
#   друг-2 10.2.0.2 ─┬ роутер2 (NAT) ──┘
#   друг-3 10.2.0.3 ─┘
#
# Так выглядит компания из четверых: двое дома у хоста, двое в другом доме.
# Все трое друзей подключаются по одному коду приглашения одновременно.
#
# Роутеры — обычный маскарадинг iptables и межсетевой экран, отбрасывающий
# непрошеные входящие пакеты, как у большинства домашних роутеров. Экран важен:
# без него роутер заводит запись conntrack на «пробивной» пакет собеседника,
# пришедший раньше нашего, и потом выдаёт нам другой внешний порт.
# Режим `symmetric` делает NAT гостя «симметричным» (--random-fully): так
# ведут себя некоторые мобильные операторы, и пробить такое без посредника нельзя.
#
#   sudo scripts/nat-lab/nat-lab.sh [cone|symmetric]
#
# LAB_ONLY=1 — только поднять сеть и сервисы (для ручной отладки), не удалять.
# Потом: ip netns exec vwf-h1 ... ; убрать — LAB_ONLY=1 ... cleanup.
#
# Нужны собранные `cargo build`: poigrulkiny-server и poigrulkiny-cli.
set -euo pipefail

MODE="${1:-cone}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${BIN:-$ROOT/target/debug}"
WORK="$(mktemp -d)"
LAB="$(dirname "$0")"

cleanup() {
  for ns in vwf-inet vwf-r1 vwf-h1 vwf-h2 vwf-r2 vwf-g1 vwf-g2; do
    ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null || true
    ip netns del "$ns" 2>/dev/null || true
  done
}
if [ "$MODE" = cleanup ]; then cleanup; exit 0; fi
[ -z "${LAB_ONLY:-}" ] && trap cleanup EXIT
cleanup

at() { local ns="$1"; shift; ip netns exec "vwf-$ns" "$@"; }

for ns in inet r1 h1 h2 r2 g1 g2; do ip netns add "vwf-$ns"; at "$ns" ip link set lo up; done

# «Интернет»: мост с адресами сервисов.
at inet ip link add br0 type bridge
at inet ip addr add 203.0.113.1/24 dev br0
at inet ip addr add 203.0.113.100/24 dev br0
at inet ip addr add 203.0.113.101/24 dev br0
at inet ip link set br0 up

# Роутер N: wan в «интернете», lan — мост, к которому подключены компьютеры.
router() {
  local r="$1" wan_ip="$2" lan_net="$3" masq="$4"; shift 4
  ip link add "$r-wan" netns "vwf-$r" type veth peer name "$r-br" netns vwf-inet
  at inet ip link set "$r-br" master br0 up
  at "$r" ip addr add "$wan_ip/24" dev "$r-wan"
  at "$r" ip link set "$r-wan" up
  at "$r" ip link add lan type bridge
  at "$r" ip addr add "$lan_net.1/24" dev lan
  at "$r" ip link set lan up
  at "$r" sysctl -qw net.ipv4.ip_forward=1
  at "$r" iptables -t nat -A POSTROUTING -o "$r-wan" -j MASQUERADE $masq
  for chain in INPUT FORWARD; do
    at "$r" iptables -A "$chain" -i "$r-wan" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
    at "$r" iptables -A "$chain" -i "$r-wan" -j DROP
  done
  local n=2 pc
  for pc in "$@"; do
    ip link add "$pc-up" netns "vwf-$r" type veth peer name "$pc-eth" netns "vwf-$pc"
    at "$r" ip link set "$pc-up" master lan up
    at "$pc" ip addr add "$lan_net.$n/24" dev "$pc-eth"
    at "$pc" ip link set "$pc-eth" up
    at "$pc" ip route add default via "$lan_net.1"
    n=$((n + 1))
  done
}
router r1 203.0.113.11 10.1.0 "" h1 h2
case "$MODE" in
  cone) router r2 203.0.113.12 10.2.0 "" g1 g2 ;;
  symmetric) router r2 203.0.113.12 10.2.0 "--random-fully" g1 g2 ;;
  *) echo "режим: cone или symmetric"; exit 2 ;;
esac

at inet python3 "$LAB/stun_server.py" 3478 203.0.113.100 203.0.113.101 &
at inet python3 "$LAB/mailbox_server.py" 203.0.113.100 8080 2>"$WORK/mailbox.log" &
sleep 1

export VWF_STUN_SERVERS="203.0.113.100:3478,203.0.113.101:3478"
export VWF_SIGNAL_URL="http://203.0.113.100:8080"
if [ -n "${LAB_ONLY:-}" ]; then
  echo "стенд готов: хост — vwf-h1, друзья — vwf-h2, vwf-g1, vwf-g2; переменные:"
  echo "  VWF_STUN_SERVERS=$VWF_STUN_SERVERS VWF_SIGNAL_URL=$VWF_SIGNAL_URL"
  exit 0
fi

# Хост за роутером 1.
at h1 env RUST_LOG=info "$BIN/poigrulkiny-server" --internet --no-discovery \
  --data-dir "$WORK/server" >"$WORK/server.log" 2>&1 &
for _ in $(seq 50); do grep -q "poi-" "$WORK/server.log" 2>/dev/null && break; sleep 0.1; done
INVITE="$(grep -o 'poi-[A-Za-z0-9_-]*' "$WORK/server.log" | head -1)"
echo "код приглашения: $INVITE"

# Друзья: один за роутером хоста, двое за роутером 2 (звук — фиктивное
# устройство ALSA). Подключаются почти одновременно и сидят в комнате 20 с.
mkdir -p "$WORK/home"
printf 'pcm.!default { type null }\nctl.!default { type hw card 0 }\n' >"$WORK/home/.asoundrc"
guest() {
  local ns="$1" name="$2"
  (sleep 20; echo) | at "$ns" env HOME="$WORK/home" RUST_LOG=warn timeout -s INT 40 \
    "$BIN/poigrulkiny-cli" connect "$INVITE" --name "$name" >"$WORK/$ns.log" 2>&1 || true
}
pids=()
guest h2 Сосед & pids+=($!); sleep 0.5
guest g1 Друг & pids+=($!); sleep 0.5
guest g2 Подруга & pids+=($!)
wait "${pids[@]}"

echo "--- сервер:"; sed 's/\x1b\[[0-9;]*m//g' "$WORK/server.log" | grep -E "внешний|гость|\+ |- " || true
ok=0
for ns in h2 g1 g2; do
  echo "--- $ns:"; grep -vE "сбоев" "$WORK/$ns.log" | grep -vE "^\s*$" | head -8
  grep -q "Подключено" "$WORK/$ns.log" && ok=$((ok + 1))
done
echo "ИТОГ ($MODE): подключились $ok из 3"

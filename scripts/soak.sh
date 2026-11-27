#!/usr/bin/env bash
# Soak test de Alfone: ejecuta una instancia (servidor o cliente) durante
# varias horas y vigila que siga viva, contando las reconexiones.
#
#   En el PC A:  ./scripts/soak.sh server --pin 123456
#   En el PC B:  ./scripts/soak.sh client --server 192.168.1.20:24800 --pin 123456
#
# Opciones comunes: --seconds N (para pruebas cortas; por defecto 24 h),
# --binary RUTA, --logdir RUTA. El resto de argumentos se pasan a `alfone`.
set -euo pipefail

usage() {
  cat <<'EOF'
Uso: soak.sh <server|client> [opciones] [-- opciones-para-alfone]

  server            ejecuta `alfone serve` (PC con teclado/ratón)
  client            ejecuta `alfone connect --reconnect`
  --seconds N       duración en segundos (por defecto 86400 = 24 h)
  --binary RUTA     binario `alfone` (por defecto ./target/debug/alfone)
  --logdir RUTA     directorio de logs (por defecto ./logs/soak)
  --help            muestra esta ayuda

Ejemplo en el PC B:
  ./scripts/soak.sh client --server 192.168.1.20:24800 --pin 123456 --seconds 600
EOF
}

role="${1:-}"
if [[ "$role" == "-h" || "$role" == "--help" || -z "$role" ]]; then
  usage
  exit 0
fi
shift

case "$role" in
  server|client) ;;
  *) echo "rol desconocido: $role" >&2; usage >&2; exit 2 ;;
esac

seconds=86400
binary="./target/debug/alfone"
logdir="./logs/soak"
alfone_args=()

while (($#)); do
  case "$1" in
    --seconds) seconds="$2"; shift 2 ;;
    --binary) binary="$2"; shift 2 ;;
    --logdir) logdir="$2"; shift 2 ;;
    --help) usage; exit 0 ;;
    --) shift; alfone_args+=("$@"); break ;;
    *) alfone_args+=("$1"); shift ;;
  esac
done

if ! [[ "$seconds" =~ ^[1-9][0-9]*$ ]]; then
  echo "--seconds debe ser un entero positivo" >&2
  exit 2
fi
if [[ ! -x "$binary" ]]; then
  echo "binario no ejecutable: $binary (usa --binary o compila con cargo build -p alfone-cli)" >&2
  exit 2
fi

mkdir -p "$logdir"
log="$logdir/$role-$(date +%Y%m%d-%H%M%S).log"
summary="$logdir/$role-summary.txt"

if [[ "$role" == "server" ]]; then
  cmd=("$binary" serve "${alfone_args[@]}")
else
  # The point of the soak test is that the client survives drops by itself.
  cmd=("$binary" connect --reconnect "${alfone_args[@]}")
fi

started=$(date +%s)
echo "iniciando soak test ($role, ${seconds}s)"
echo "  comando: ${cmd[*]}"
echo "  log:     $log"
"${cmd[@]}" >"$log" 2>&1 &
pid=$!

poll=60
if ((seconds < poll)); then poll="$seconds"; fi
end=$((started + seconds))
last_reconnects=0

while kill -0 "$pid" 2>/dev/null; do
  now=$(date +%s)
  if ((now >= end)); then
    echo "duración completada; deteniendo $role ..."
    break
  fi

  reconnects=$(grep -c 'reconnecting in' "$log" 2>/dev/null || true)
  if ((reconnects > last_reconnects)); then
    echo "  $(date -u +%H:%M:%SZ) reconexiones: $reconnects"
    last_reconnects="$reconnects"
  fi

  if [[ "$role" == "server" ]] && command -v curl >/dev/null; then
    token=$(grep -o 'http token:  *[0-9a-f]*' "$log" 2>/dev/null | awk '{print $NF}' | tail -1 || true)
    if [[ -n "$token" ]]; then
      health=$(curl -fsS -m 3 -H "Authorization: Bearer $token" \
        http://127.0.0.1:24801/health 2>/dev/null || true)
      if [[ -z "$health" ]]; then
        echo "  $(date -u +%H:%M:%SZ) AVISO: /health no responde" >&2
      fi
    fi
  fi

  sleep "$poll"
done

if kill -0 "$pid" 2>/dev/null; then
  kill -INT "$pid" 2>/dev/null || true
  for _ in 1 2 3 4 5; do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
  done
  kill -KILL "$pid" 2>/dev/null || true
fi
wait "$pid" 2>/dev/null || true

elapsed=$(( $(date +%s) - started ))
reconnects=$(grep -c 'reconnecting in' "$log" 2>/dev/null || true)
errors=$(grep -ciE 'error|panic|session ended' "$log" 2>/dev/null || true)
{
  echo "role:        $role"
  echo "started:     $(date -u -d "@$started" +%FT%TZ 2>/dev/null || echo "$started")"
  echo "elapsed_s:   $elapsed"
  echo "reconnects:  $reconnects"
  echo "errors:      $errors"
  echo "log:         $log"
} >"$summary"
cat "$summary"

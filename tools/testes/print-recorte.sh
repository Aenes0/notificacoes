#!/bin/sh
# Teste de integração do ~/.local/bin/print-recorte contra o daemon `notificacoes --sem-tela`
# num barramento de sessão PRIVADO (não toca o daemon da sessão nem a tela). slurp, grim, wl-copy,
# satty, xdg-open e xdg-user-dir são executáveis de mentira (tools/testes/stubs) que registram em $STUB_LOG.
# Uso: sh tools/testes/print-recorte.sh   (a partir de qualquer pasta; roda no host)
# Sai 0 se todos os casos passam.

AQUI=$(cd "$(dirname "$0")" && pwd)
RAIZ=$(cd "$AQUI/../.." && pwd)
SCRIPT="${PRINT_RECORTE_BIN:-$HOME/.local/bin/print-recorte}"   # resolvido antes de trocar o HOME
DAEMON="${NOTIFICACOES_BIN:-$RAIZ/target/debug/notificacoes}"
PNG="$RAIZ/tests/fixtures/images/print.png"
PRAZO=3000

TMP=$(mktemp -d)
FAKEHOME="$TMP/home"
mkdir -p "$FAKEHOME/.cache" "$TMP/cfg"
export STUB_LOG="$TMP/stub.log" STUB_PNG="$PNG"
: > "$STUB_LOG"

PIDS=""
limpar() { for p in $PIDS; do kill "$p" 2>/dev/null; done; pkill -f "expire-time=$PRAZO" 2>/dev/null; rm -rf "$TMP"; }
trap limpar EXIT INT TERM

# Barramento privado
dbus-daemon --session --nofork --print-address=1 > "$TMP/addr" 2> "$TMP/dbus.err" &
PIDS="$PIDS $!"
i=0; while [ ! -s "$TMP/addr" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
DBUS_SESSION_BUS_ADDRESS=$(head -n1 "$TMP/addr")
export DBUS_SESSION_BUS_ADDRESS
[ -n "$DBUS_SESSION_BUS_ADDRESS" ] || { echo "FALHA barramento privado não subiu"; exit 1; }

# Daemon sem tela
XDG_CONFIG_HOME="$TMP/cfg" NOTIFICACOES_DEBUG=1 "$DAEMON" --sem-tela 2> "$TMP/daemon.log" &
PIDS="$PIDS $!"
i=0
until busctl --user call org.freedesktop.Notifications /org/freedesktop/Notifications \
        org.freedesktop.Notifications GetServerInformation > /dev/null 2>&1; do
    i=$((i + 1)); [ $i -gt 50 ] && { echo "FALHA daemon não tomou o nome"; cat "$TMP/daemon.log"; exit 1; }
    sleep 0.1
done

dbus-monitor --session > "$TMP/mon.log" 2>&1 &
PIDS="$PIDS $!"
sleep 0.5

# PATH só com os stubs e as ferramentas necessárias: sem o satty real no caso "sem satty".
ferr="$TMP/bin-sem-satty"; mkdir -p "$ferr"
for t in sh date mkdir pkill od setsid notify-send cat cp sleep; do ln -s "$(command -v $t)" "$ferr/$t"; done
for s in slurp grim wl-copy xdg-open xdg-user-dir; do ln -s "$AQUI/stubs/$s" "$ferr/$s"; done

FALHAS=0
ok()    { echo "ok $1"; }
falha() { echo "FALHA $1${2:+: $2}"; FALHAS=$((FALHAS + 1)); }

# captura [PATH]: roda o print-recorte com HOME falso; devolve em $ID o id da notificação nova.
captura() {
    antes=$(grep -c 'notify id=' "$TMP/daemon.log")
    HOME="$FAKEHOME" PATH="${1:-$AQUI/stubs:$PATH}" PRINT_RECORTE_PRAZO_MS=$PRAZO sh "$SCRIPT"
    RC=$?
    i=0
    while [ "$(grep -c 'notify id=' "$TMP/daemon.log")" -le "$antes" ]; do
        i=$((i + 1)); [ $i -gt 50 ] && { ID=""; return 1; }
        sleep 0.1
    done
    ID=$(grep 'notify id=' "$TMP/daemon.log" | tail -n1 | sed 's/.*notify id=\([0-9]*\).*/\1/')
    ARQ=$(grep '^grim ' "$STUB_LOG" | tail -n1 | awk '{print $NF}')
}

invocar() { dbus-send --session --dest=org.freedesktop.Notifications --type=method_call \
    /com/system76/NotificationsApplet com.system76.NotificationsApplet.InvokeAction uint32:"$1" string:"$2"; }
fechar() { dbus-send --session --dest=org.freedesktop.Notifications --type=method_call \
    /org/freedesktop/Notifications org.freedesktop.Notifications.CloseNotification uint32:"$1"; }
ajudante_vivo() { pgrep -f "expire-time=$PRAZO" > /dev/null; }
espera_log() {  # espera até o STUB_LOG casar o padrão (grep -F), até ~3 s
    i=0; while ! grep -qF -- "$1" "$STUB_LOG"; do i=$((i + 1)); [ $i -gt 30 ] && return 1; sleep 0.1; done
}
zerar() { : > "$STUB_LOG"; pkill -f "expire-time=$PRAZO" 2>/dev/null; sleep 0.3; }

SATTY_ARGS="--initial-tool arrow --copy-command wl-copy --actions-on-enter save-to-clipboard,save-to-file,exit --early-exit all --disable-notifications"

# 1. Notify com imagem, ícones de ação e três ações
zerar
if captura && [ -n "$ID" ]; then
    sleep 0.3
    m="$TMP/mon.log"; alvo="$FAKEHOME/Imagens/Screenshots/"
    if grep -q 'string "image-path"' "$m" && grep -qF "${alvo}Captura_" "$m" \
        && grep -q 'string "action-icons"' "$m" && grep -q 'string "default"' "$m" \
        && grep -q 'string "editar"' "$m" && grep -q 'string "folder-open-symbolic"' "$m" \
        && grep -qF '800×450 · salvo em Screenshots' "$m" && grep -qF 'Captura copiada' "$m" \
        && grep -q 'string "sound-name"' "$m"; then
        ok print_recorte_sends_image_notify
    else falha print_recorte_sends_image_notify "dbus-monitor sem as dicas esperadas"; fi
else falha print_recorte_sends_image_notify "nenhuma notificação chegou ao daemon"; fi
invocar "$ID" nenhuma > /dev/null 2>&1; fechar "$ID" > /dev/null 2>&1

# 2. editar -> satty
zerar
if captura && invocar "$ID" editar && espera_log "satty --filename $ARQ --output-filename $ARQ $SATTY_ARGS"; then
    ok print_recorte_editar_runs_satty
else falha print_recorte_editar_runs_satty "$(cat "$STUB_LOG")"; fi

# 3. default -> satty
zerar
if captura && invocar "$ID" default && espera_log "satty --filename $ARQ --output-filename $ARQ $SATTY_ARGS"; then
    ok print_recorte_default_runs_satty
else falha print_recorte_default_runs_satty "$(cat "$STUB_LOG")"; fi

# 4. pasta -> xdg-open da pasta
zerar
if captura && invocar "$ID" folder-open-symbolic && espera_log "xdg-open $FAKEHOME/Imagens/Screenshots"; then
    ok print_recorte_folder_opens_dir
else falha print_recorte_folder_opens_dir "$(cat "$STUB_LOG")"; fi

# 5. fechar (×) não abre nada e o ajudante termina
zerar
if captura; then
    fechar "$ID"; sleep 1
    if ! grep -qE '^(satty|xdg-open) ' "$STUB_LOG" && ! ajudante_vivo; then ok print_recorte_close_does_nothing
    else falha print_recorte_close_does_nothing "$(cat "$STUB_LOG")"; fi
else falha print_recorte_close_does_nothing "nenhuma notificação"; fi

# 6. sem ação: o ajudante morre pelo prazo
zerar
if captura; then
    sleep $((PRAZO / 1000 + 1))
    if ! ajudante_vivo && ! grep -qE '^(satty|xdg-open) ' "$STUB_LOG"; then ok print_recorte_helper_dies_after_deadline
    else falha print_recorte_helper_dies_after_deadline "ajudante ainda vivo ou ação indevida"; fi
else falha print_recorte_helper_dies_after_deadline "nenhuma notificação"; fi

# 7. duas capturas independentes: a ação na 2ª só abre a 2ª
zerar
if captura; then
    ID1=$ID; ARQ1=$ARQ; sleep 1.2
    if captura && [ "$ID" != "$ID1" ] && [ "$ARQ" != "$ARQ1" ] && invocar "$ID" editar \
        && espera_log "satty --filename $ARQ " && sleep 0.5 \
        && ! grep -qF "satty --filename $ARQ1 " "$STUB_LOG" && [ "$(grep -c '^satty ' "$STUB_LOG")" = 1 ]; then
        ok print_recorte_two_captures_independent
    else falha print_recorte_two_captures_independent "$(cat "$STUB_LOG")"; fi
    fechar "$ID1" > /dev/null 2>&1
else falha print_recorte_two_captures_independent "nenhuma notificação"; fi

# 8. editar sem satty instalado: linha no log e o script sai 0
zerar
: > "$FAKEHOME/.cache/print-recorte.log"
if captura "$ferr" && [ "$RC" = 0 ] && invocar "$ID" editar; then
    sleep 1
    if grep -q 'satty' "$FAKEHOME/.cache/print-recorte.log"; then ok print_recorte_editar_without_satty_logs
    else falha print_recorte_editar_without_satty_logs "log: $(cat "$FAKEHOME/.cache/print-recorte.log")"; fi
else falha print_recorte_editar_without_satty_logs "captura falhou (rc=$RC)"; fi

[ "$FALHAS" -eq 0 ]

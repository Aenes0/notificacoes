#!/bin/sh
# Modo dev: roda o daemon de notificações no lugar do oficial, sem instalar.
#   tools/dev.sh iniciar   compila (release) e inicia; o nome passa para o nosso daemon
#   tools/dev.sh parar     encerra o nosso e devolve o nome ao cosmic-notifications oficial
#   tools/dev.sh status    dono do nome e PIDs
# O nosso pede o nome com ReplaceExisting e o oficial o cede (spike: nome_tomado_do_oficial = sim),
# então `iniciar` não precisa matar o oficial. Mas o oficial não pega o nome de volta sozinho:
# `parar` o mata e a sessão o religa (o painel pisca).
set -e

cd "$(dirname "$0")/.."
LOG="$HOME/.cache/notificacoes-dev.log"
PIDFILE="$HOME/.cache/notificacoes-dev.pid"
NOME=org.freedesktop.Notifications

dono() {
    busctl --user status "$NOME" 2>/dev/null | grep -E "^PID|^Comm" || echo "sem dono de $NOME"
}

nosso_pid() {
    [ -f "$PIDFILE" ] || return 1
    pid=$(cat "$PIDFILE")
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && echo "$pid"
}

case "${1:-}" in
    iniciar)
        if pid=$(nosso_pid); then
            echo "já rodando (PID $pid)"
            dono
            exit 0
        fi
        cargo build --release
        mkdir -p "$HOME/.cache"
        setsid target/release/notificacoes >"$LOG" 2>&1 </dev/null &
        echo $! >"$PIDFILE"
        sleep 1
        echo "notificacoes PID $(cat "$PIDFILE"), log em $LOG"
        dono
        ;;
    parar)
        if pid=$(nosso_pid); then
            kill "$pid"
            echo "notificacoes (PID $pid) encerrado"
        else
            echo "notificacoes não estava rodando"
        fi
        rm -f "$PIDFILE"
        # O nome do processo é cortado em 15 caracteres: `pkill -x cosmic-notifications` não acha.
        oficial=$(pidof cosmic-notifications || true)
        if [ -n "$oficial" ]; then
            kill $oficial
            echo "cosmic-notifications oficial (PID $oficial) encerrado; a sessão o religa"
        fi
        sleep 2
        dono
        ;;
    status)
        dono
        if pid=$(nosso_pid); then echo "notificacoes PID $pid"; else echo "notificacoes parado"; fi
        echo "cosmic-notifications PID $(pidof cosmic-notifications || echo nenhum)"
        ;;
    *)
        echo "uso: $0 iniciar|parar|status" >&2
        exit 2
        ;;
esac

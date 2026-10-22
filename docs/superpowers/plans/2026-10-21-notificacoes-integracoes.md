# notificacoes (integrações) — Plano de Implementação

> **Para agentes:** SUB-SKILL OBRIGATÓRIA: use `superpowers:subagent-driven-development` (recomendado) ou `superpowers:executing-plans` para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** fazer os produtores e o consumidor de avisos falarem com o daemon novo: o `print-recorte` manda `Notify` com imagem e ações (aposentando o `print-preview`), o logi-osd para de desenhar e manda `Notify`, a Central ignora avisos `transient` e aciona ações pelo método do daemon no barramento de sessão, e (condicional) o fork do greeter informa o bloqueio ao logind.

**Arquitetura:** nenhum protocolo novo — tudo é `org.freedesktop.Notifications.Notify` com as dicas definidas no plano do daemon, mais `com.system76.NotificationsApplet.InvokeAction` em `/com/system76/NotificationsApplet` no mesmo nome de barramento. Cada mudança vive no repositório dela; os testes automáticos usam um barramento privado (`dbus-daemon --session --nofork --print-address=1`) e, quando preciso, o daemon em `--sem-tela`.

**Tech stack:** shell POSIX + `notify-send` 0.8.9 (libnotify) no `print-recorte`; Rust 2024 + `dbus 0.9.12` + `calloop 0.14` no logi-osd; zbus 5 (já usado) na Central; `logind-zbus` (já usado) no greeter.

**Spec:** `docs/superpowers/specs/2026-10-21-notificacoes-design.md` §3 (e §2.11 para a T4). **Depende de:** `docs/superpowers/plans/2026-10-21-notificacoes-daemon.md` — Interfaces usadas aqui: nomes de dicas e regras das Global Constraints, `InvokeAction` (T5), modo `--sem-tela` e `NOTIFICACOES_DEBUG=1` (T5), `tests/common/mod.rs::PrivateBus` (T5, modelo para o `test_bus.rs` do logi-osd), imagem grande com `action-icons` (T11), pílula (T16), `tools/dev.sh` (T9).

## Pré-requisitos (feitos pelo USUÁRIO, não pelo agente)

1. Para cada repositório tocado, o controller **pergunta a branch** antes de editar (CLAUDE.md global): `~/Projetos/quick-settings` (hoje `main`), `~/Projetos/logi-osd` (hoje `main`), `~/Projetos/cosmic-greeter` (hoje `master`, só se a T4 for aprovada). `~/.local/bin` não é git: a T2 faz cópia de segurança antes de editar.
2. `H '<cmd>'` = `flatpak-spawn --host sh -c '<cmd>'` (sempre com `cd` explícito para o repositório da tarefa).
3. Ordem de execução: T1 depois da T5 do daemon; T2 depois da T11 do daemon; T3 depois da T16 do daemon; T4 depois da T13 do daemon; T5 por último, com o pacote do daemon instalado (T17).

## Regras de execução (toda tarefa herda)

- **Ninguém commita.** Todo passo final é "Deixar as mudanças no working tree (não commitar)". Implementers recebem "não commite".
- Tier marcado no título; revisor sempre `fable`.
- **Licença de corrigir o plano:** se o código do brief não compilar, contradisser as assinaturas reais (zbus, dbus, calloop, logind-zbus, libnotify) ou falhar nos testes do próprio brief, o plano pode estar errado — corrija para satisfazer o teste e relate a divergência; nunca altere o teste para acomodar implementação defeituosa.
- Nunca apagar arquivo do usuário fora dos repositórios (o `print-preview` fica onde está; a remoção é do usuário).

## Global Constraints

- Dicas e valores exatos (definidos no plano do daemon): `x-estilo` = `osd`\|`cartao`\|`imagem`\|`compacto`; `x-tom` = `normal`\|`atencao`\|`critico`\|`apagado`; `x-pontos`, `x-pontos-total` (int); `transient` (bool); `synchronous` (string); `category`; `sound-name`; `image-path`; `action-icons` (bool).
- `NotificationClosed`: motivo 2 em × e em clique/ação (após `ActionInvoked`), 3 em `CloseNotification`, 1 só para `transient` que expirou; expirar um aviso comum **não emite nada**.
- `InvokeAction`: destino `org.freedesktop.Notifications`, caminho `/com/system76/NotificationsApplet`, interface `com.system76.NotificationsApplet`, assinatura `(u id, s action)`.
- Vocabulário de ícones da pílula: `logi-caps-lock`, `logi-num-lock`, `logi-insert`, `logi-fn-lock`, `logi-backlight`, `logi-battery`, `logi-battery-charging`, `logi-switch`.
- Regras do spec do logi-osd continuam: avisa bateria uma vez a 15% e a 5%, nunca empilha pílulas, não avisa ao desconectar; textos pt-BR da tabela do plano do logi-osd inalterados.
- Satty com exatamente os argumentos de hoje do `print-preview`: `satty --filename F --output-filename F --initial-tool arrow --copy-command wl-copy --actions-on-enter save-to-clipboard,save-to-file,exit --early-exit all --disable-notifications`.
- Logi-osd após a migração: sem `smithay-client-toolkit`, `tiny-skia`, `ab_glyph`; mesmo perfil release; meta de repouso igual ou melhor que a medida (4,2 MB, 3 threads, 0 acordadas).

## Review Focus

1. **Prints em sequência** (`Win+Shift+S` três vezes seguidas) → cada captura tem o seu aviso e o seu `notify-send --wait` independente; nenhum fica vivo mais que o prazo (10 min). Teste: `print_recorte_two_captures_independent` (T2).
2. **Daemon ausente ou trocando de dono** (sessão com o oficial, `tools/dev.sh parar`, crash do daemon) quando o logi-osd manda aviso → o logi-osd não trava, não cai, e volta a funcionar sozinho. Teste: `send_without_daemon_does_not_block` e `reconnects_after_bus_error` (T3).
3. **`transient` com tipo inesperado** (`int32 1`, `byte 1`) ou pílula sem `transient` → a Central aplica a mesma regra do daemon e não guarda pílulas. Teste: `history_skips_transient_and_osd` (T1).
4. **Latência da pílula** (Caps Lock 10× rápido) → evento→commit < 50 ms somando logi-osd + D-Bus + daemon. Verificação medida na T3 e na T5.
5. **Ação do print com Satty ausente ou pasta com espaço/acentos** → erro no log do `print-recorte`, nada trava, o próximo print funciona. Teste: `print_recorte_editar_without_satty_logs` (T2).

---

## Tarefas

1. [sonnet] Central — `transient` no histórico e `InvokeAction` pelo barramento
2. [sonnet] print-recorte — `Notify` com imagem, ações e prazo
3. [opus] logi-osd — `Notify` no lugar da camada `osd/`
4. [sonnet] (Condicional) greeter — `SetLockedHint` no bloqueio
5. [sonnet] Aceite integrado — roteiro fechado

---

### Tarefa 1: [sonnet] Central — `transient` no histórico e `InvokeAction` pelo barramento

**Files:**
- Modify: `~/Projetos/quick-settings/src/notifications.rs` (doc do módulo; proxy `NotificationsApplet`; `daemon()` removida; `bus_notifications` filtra; `invoke`; testes)

**Interfaces:**
- Consumes: `InvokeAction` e regras de `transient` das Global Constraints; daemon em modo dev (`tools/dev.sh`, plano do daemon T9) para a verificação ao vivo.
- Produces:
  ```rust
  /// Fica fora do histórico: `transient` verdadeiro (b true, ou inteiro ≠ 0) ou `x-estilo` = "osd".
  fn skip_in_history(hints: &HashMap<String, zbus::zvariant::OwnedValue>) -> bool;
  #[zbus::proxy(default_service = "org.freedesktop.Notifications", interface = "com.system76.NotificationsApplet",
                default_path = "/com/system76/NotificationsApplet")]
  trait NotificationsApplet { fn invoke_action(&self, id: u32, action: &str) -> zbus::Result<()>; }
  pub async fn invoke(id: u32, action: String);  // mesma assinatura; agora pela conexão de sessão
  ```
- No ramo `MethodCall` de `bus_notifications`, depois de desserializar: `if skip_in_history(&hints) { return None; }` (não entra em `waiting`). Remover `OnceCell`, `FromRawFd`, `RawFd`, a leitura de `COSMIC_NOTIFICATIONS` e o sinal `notify` do proxy antigo. Atualizar o comentário do módulo (ações vão pelo método do daemon, não pelo socket do painel).

- [ ] **Passo 1: Teste que falha** (no `mod tests` existente)

```rust
#[test]
fn history_skips_transient_and_osd() {
    // b true, i32 1, u8 1 → pula; b false, i32 0 → guarda; x-estilo "osd" → pula; "cartao" → guarda; sem dicas → guarda
}
```
- [ ] **Passo 2:** `H 'cd ~/Projetos/quick-settings && cargo test history_skips'` → FAIL (função inexistente). **Passo 3:** implementar. **Passo 4:** `H 'cd ~/Projetos/quick-settings && cargo test'` → PASS (os testes `markup`, `popups_closed`, `sent_icon` continuam verdes).
- [ ] **Passo 5:** `H 'cd ~/Projetos/quick-settings && ./install.sh'` → instalado; o painel recarrega o applet (ou o usuário remove/adiciona a Central).
- [ ] **Passo 6 (ao vivo, com o usuário):** `H 'cd ~/Projetos/notificacoes && tools/dev.sh iniciar; notify-send -e efemera; notify-send -A default=Abrir -A b=Botão Persistente texto & sleep 8; dbus-monitor --session "interface=org.freedesktop.Notifications,type=signal" > ~/Projetos/notificacoes/docs/verificacao/i1-sinais.log & sleep 1'`. Abrir a Central: `efemera` **não** está; `Persistente` está com o botão "Botão". Clicar "Botão" → o log tem `ActionInvoked(id, "b")` e `NotificationClosed(id, 2)`, e o `notify-send` em segundo plano imprime `b` e termina. Depois `tools/dev.sh parar`.
- [ ] **Passo 7: Deixar no working tree (não commitar)** — `~/Projetos/quick-settings/src/notifications.rs`.

---

### Tarefa 2: [sonnet] print-recorte — `Notify` com imagem, ações e prazo

O "ajudante que escuta `ActionInvoked` com prazo" é o próprio `notify-send --wait` em segundo plano: no libnotify 0.8.9 (`tools/notify-send.c`) `-t` vira também o prazo de espera do `--wait` (`g_timeout_add(expire_timeout, on_wait_timeout)`), ele imprime a chave da ação invocada e sai no `NotificationClosed`. O daemon ignora o `-t 600000` para a exibição (maior que o tempo do nível) e usa 6 s.

**Files:**
- Modify: `~/.local/bin/print-recorte` (antes: `cp ~/.local/bin/print-recorte ~/.local/bin/print-recorte.antes-notificacoes`)
- Create: `~/Projetos/notificacoes/tools/testes/print-recorte.sh` (teste de integração), `~/Projetos/notificacoes/tools/testes/stubs/{slurp,grim,wl-copy,satty,xdg-open,xdg-user-dir}` (executáveis de mentira que registram a chamada em `$STUB_LOG`), `~/Projetos/notificacoes/tests/fixtures/images/print.png` (já existe, da T11 do daemon)

**Interfaces:**
- Consumes: daemon `--sem-tela` e `NOTIFICACOES_DEBUG=1` (daemon T5; o barramento privado é subido no shell, como o `PrivateBus` faz); formato imagem com `action-icons` (daemon T11).
- Produces (comportamento do script):
  - Depois de `grim` e `wl-copy`, lê largura e altura do cabeçalho PNG (`od -An -tu1 -j16 -N8 "$FILE"`, big-endian) e chama, em segundo plano (`( … ) &`):
    ```sh
    acao=$(notify-send --app-name="Captura de tela" --icon=applets-screenshooter \
        --expire-time="${PRINT_RECORTE_PRAZO_MS:-600000}" \
        --hint=string:image-path:"$FILE" --hint=boolean:action-icons:true \
        --hint=string:sound-name:screen-capture \
        --action=default=Editar --action=editar=Editar --action=folder-open-symbolic="Abrir pasta" \
        --wait "Captura copiada" "${W}×${H} · salvo em Screenshots")
    ```
  - `default`/`editar` → `satty …` (argumentos das Global Constraints) com `setsid -f`; `folder-open-symbolic` → `setsid -f xdg-open "$DIR"`; vazio (×, prazo) → nada. Comando ausente → linha no log (`~/.cache/print-recorte.log`) e segue.
  - A chamada ao `print-preview` sai. O arquivo `print-preview` fica (o usuário apaga quando quiser).
- `tools/testes/print-recorte.sh`: sobe `PrivateBus` (`dbus-daemon --session --nofork --print-address=1`), o daemon `--sem-tela` com `XDG_CONFIG_HOME` temporário, `HOME` temporário, `PATH="$stubs:$PATH"`, `PRINT_RECORTE_PRAZO_MS=3000`; roda `~/.local/bin/print-recorte`; acha o id pelo `dbus-monitor` (ou pelo log do daemon com `NOTIFICACOES_DEBUG=1`, que imprime `notify id=<n> app=<…>`); chama `InvokeAction` com `dbus-send`; confere `$STUB_LOG`. Sai 0 se todos os casos passam, imprimindo `ok <caso>`/`FALHA <caso>`.

- [ ] **Passo 1: Teste que falha** — escrever `tools/testes/print-recorte.sh` com os casos:

```text
print_recorte_sends_image_notify        # dbus-monitor mostra Notify com image-path=<arquivo>, action-icons true, ações default/editar/folder-open-symbolic, corpo "800×450 · salvo em Screenshots"
print_recorte_editar_runs_satty         # InvokeAction(id,"editar") → STUB_LOG tem "satty --filename <F> --output-filename <F> --initial-tool arrow …"
print_recorte_default_runs_satty        # InvokeAction(id,"default") → idem
print_recorte_folder_opens_dir          # InvokeAction(id,"folder-open-symbolic") → "xdg-open <HOME>/Imagens/Screenshots"
print_recorte_close_does_nothing        # CloseNotification(id) → nenhum satty/xdg-open; notify-send terminou
print_recorte_helper_dies_after_deadline # sem ação: após PRINT_RECORTE_PRAZO_MS + 1 s, nenhum notify-send vivo
print_recorte_two_captures_independent  # duas execuções; ação na 2ª só abre a 2ª
print_recorte_editar_without_satty_logs # stub satty removido do PATH → linha no log, script sai 0
```
- [ ] **Passo 2:** `H 'cd ~/Projetos/notificacoes && sh tools/testes/print-recorte.sh'` → FAIL (o script ainda chama o `print-preview`).
- [ ] **Passo 3:** cópia de segurança e implementar a mudança no `print-recorte` (manter: log, `pkill -x slurp`, `exec < /dev/null`, `slurp` com as mesmas cores, nome do arquivo, `wl-copy`, aviso crítico se o `grim` falhar). **Passo 4:** `H 'sh -n ~/.local/bin/print-recorte && cd ~/Projetos/notificacoes && sh tools/testes/print-recorte.sh'` → todos `ok`.
- [ ] **Passo 5 (ao vivo, com o usuário):** com o daemon em modo dev, `Win+Shift+S`, recortar → aviso de imagem grande igual ao `print-preview` (miniatura, "Captura copiada", "L×A · salvo em Screenshots", "Editar" colorido, pasta redonda, × redondo, barra); clicar na miniatura → Satty abre; outro print, clicar na pasta → gerenciador de arquivos; outro, deixar expirar → nada. `grim docs/verificacao/i2-print.png` com o aviso na tela.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `~/.local/bin/print-recorte` (fora de git; a cópia `print-recorte.antes-notificacoes` é a volta), `~/Projetos/notificacoes/tools/testes/`.

---

### Tarefa 3: [opus] logi-osd — `Notify` no lugar da camada `osd/`

**Files:**
- Delete (código do próprio repositório, substituído): `~/Projetos/logi-osd/src/osd/{mod,controller,icons,render,theme,wayland}.rs`
- Create: `~/Projetos/logi-osd/src/notify.rs` (mapeamento puro + `Notifier` + testes), `~/Projetos/logi-osd/src/test_bus.rs` (`#[cfg(test)]`; mesmo `PrivateBus` do daemon — o logi-osd é só binário, então os testes de barramento ficam dentro do crate)
- Modify: `~/Projetos/logi-osd/src/main.rs` (sem `Osd`, `OsdController`, `Renderer`, `HideTimer`, `hide_decision`, `needs_rearm` e seus testes; `show` → `notifier.send`), `~/Projetos/logi-osd/src/config.rs` (`fonte` obsoleta), `~/Projetos/logi-osd/Cargo.toml`, `~/Projetos/logi-osd/data/config.exemplo` (sai o bloco `fonte`; `duracao_ms`/`duracao_critica_ms` passam a valer só para as pílulas)

**Interfaces:**
- Consumes: `Pill`, `Icon`, `Tone`, `Value` (`src/message.rs`, inalterado); dicas e vocabulário das Global Constraints; regras do daemon (pílula = `x-estilo osd`, sempre substitui; nível por `x-tom`).
- Produces:
  ```rust
  // notify.rs
  #[derive(Debug, Clone, PartialEq)] pub enum Hint { Bool(bool), Int(i32), Str(String) }
  #[derive(Debug, Clone, PartialEq)] pub struct NotifyArgs {
      pub app_name: &'static str /* "logi-osd" */, pub app_icon: &'static str, pub summary: String, pub body: String,
      pub hints: Vec<(&'static str, Hint)>, pub expire_timeout: i32,
  }
  pub fn to_notify(p: &Pill) -> NotifyArgs;
  pub struct Notifier { /* Option<Rc<LocalConnection>> compartilhada com o callback do fd, erro já logado */ }
  impl Notifier {
      /// `address`: None = barramento de sessão do ambiente; Some = endereço explícito (testes). Conecta na 1ª send.
      pub fn new(address: Option<String>) -> Notifier;
      /// Não bloqueia esperando resposta: envia a chamada (com resposta esperada, para a Central casar
      /// chamada e resposta) e volta; as respostas e erros chegam pelo fd no calloop e são descartados
      /// (erro: uma linha de log por tipo até o próximo sucesso). Falha de envio → descarta a conexão; a próxima send reconecta.
      pub fn send<S: 'static>(&mut self, handle: &LoopHandle<'static, S>, p: &Pill); // genérico: o callback só drena a conexão (Rc), não toca no estado
  }
  ```
- Mapeamento (`to_notify`):

| Pill | app_icon | summary | body | hints | expire |
|---|---|---|---|---|---|
| lock Caps/Num/Insert ligado | `logi-caps-lock`/`logi-num-lock`/`logi-insert` | `Caps Lock`… | `Ativado` | osd + `x-tom normal` | `duracao_ms` |
| lock desligado | idem | idem | `Desativado` | osd + `x-tom apagado` | `duracao_ms` |
| Fn | `logi-fn-lock` | `Fn` | `Teclas F`/`Teclas de mídia` | osd + `x-tom apagado` | `duracao_ms` |
| luz | `logi-backlight` | `Teclado` | `""` | osd + `x-pontos`, `x-pontos-total` | `duracao_ms` |
| bateria baixa | `logi-battery` | `<Nome>: bateria baixa` | `<n>%` | **cartão**: `x-tom atencao`, `category device`, `sound-name battery-low` | `-1` |
| bateria crítica | `logi-battery` | idem | `<n>%` | **cartão**: `x-tom critico`, `category device`, `sound-name battery-low` | `-1` |
| carregando / carregado | `logi-battery-charging` | `<Nome> carregando`/`carregado` | `<n>%` / `""` | osd + `x-tom apagado` | `duracao_ms` |
| conectado | `logi-switch` | `<Nome> conectado` | `Canal k` / `""` | osd + `x-tom apagado` | `duracao_ms` |

  "osd" = `x-estilo osd`, `transient true`, `synchronous logi-osd`. Cartões sem `x-estilo` e sem `transient` (vão para o histórico da Central). `expire_timeout` vem de `Pill.duration` em ms.
- `Cargo.toml`: remover `smithay-client-toolkit`, `tiny-skia`, `ab_glyph`; acrescentar `calloop = "0.14"` e `dbus = { version = "0.9.12", default-features = false }`; `rustix` sem a feature `mm`; `[dev-dependencies]` sem `tiny-skia`. Trocar `smithay_client_toolkit::reexports::calloop` por `calloop`.
- `config.rs`: chave `fonte` aceita e ignorada com aviso `linha N: chave obsoleta "fonte" (o texto agora é desenhado pelo daemon de notificações)`; campo `font` sai do `Config`. O teste `font_path_and_empty_font` é trocado por `font_key_is_obsolete_and_warns` (mudança de comportamento pedida pelo design §3.2, não afrouxamento).
- `--demo` continua: cada pílula da demo vira `Notify`; termina com `signal.stop()` como hoje.
- `LOGI_OSD_DEBUG=1` passa a imprimir `logi-osd: {ms} ms evento→envio`.

- [ ] **Passo 1: Testes que falham**

```rust
// src/notify.rs
#[test] fn caps_on_maps_to_osd_pill()        // app_icon "logi-caps-lock", summary "Caps Lock", body "Ativado", hints ⊇ {x-estilo osd, transient true, synchronous logi-osd, x-tom normal}, expire 1500
#[test] fn caps_off_is_apagado()
#[test] fn fn_lock_texts_and_icon()
#[test] fn backlight_maps_dots()             // backlight(3, 9) → x-pontos 3, x-pontos-total 8, body ""
#[test] fn battery_low_is_atencao_card()     // sem x-estilo, sem transient; x-tom atencao; category device; sound-name battery-low; expire -1; summary "MX Keys Mini: bateria baixa"; body "15%"
#[test] fn battery_critical_is_critico_card()
#[test] fn charging_and_charged_are_pills()
#[test] fn connected_with_and_without_channel()
#[test] fn every_demo_pill_maps()            // as 10 pílulas de demo_pills: nenhum app_icon fora do vocabulário
// src/config.rs
#[test] fn font_key_is_obsolete_and_warns()  // "fonte = /x.ttf" → 1 aviso com "obsoleta"; Config == padrão
// src/notify.rs, mod bus_tests (PrivateBus de test_bus.rs; o teste registra org.freedesktop.Notifications e grava as chamadas; EventLoop<()>)
#[test] fn notifier_sends_notify_to_bus()    // send(caps on) → chamada Notify com (s u s s s as a{sv} i) e os valores da tabela
#[test] fn send_without_daemon_does_not_block() // nome sem dono → send volta em < 50 ms; processo segue
#[test] fn reconnects_after_bus_error()      // Notifier com endereço de um PrivateBus que é derrubado → send loga e descarta; com o daemon do teste de volta no mesmo endereço (dbus-daemon --address=unix:path=<arquivo fixo>) → send seguinte chega
```
- [ ] **Passo 2:** `H 'cd ~/Projetos/logi-osd && cargo test'` → FAIL. **Passo 3:** implementar, apagar `src/osd/` e os testes de timer de `main.rs` (o código que testavam saiu). **Passo 4:** `H 'cd ~/Projetos/logi-osd && cargo test && cargo build --release'` → PASS; `H 'cd ~/Projetos/logi-osd && grep -rn "osd::\|tiny_skia\|ab_glyph\|smithay" src Cargo.toml'` → nada.
- [ ] **Passo 5 (ao vivo):** daemon em modo dev (ou instalado); `H 'cd ~/Projetos/logi-osd && ./install.sh'`; Caps 3×, luz +/−, Easy-Switch → pílula no centro inferior com o visual novo, uma por vez; `LOGI_OSD_DEBUG=1` no logi-osd e `NOTIFICACOES_DEBUG=1` no daemon: soma `evento→envio` + `Notify→commit` < 50 ms em 10 apertos de Caps. `grim ~/Projetos/notificacoes/docs/verificacao/i3-pilula.png`.
- [ ] **Passo 6 (repouso):** script de medição do plano do daemon (T18, 30 s) com `pid=$(pidof logi-osd)` → `ctx_30s=0`; anotar `VmRSS` e `Threads` (esperado: menor que 4,2 MB e ≤ 3 threads, sem a fonte mapeada nem buffers Wayland).
- [ ] **Passo 7: Deixar no working tree (não commitar)** — `src/notify.rs`, `src/test_bus.rs`, `src/main.rs`, `src/config.rs`, `src/osd/` (removidos), `Cargo.toml`, `Cargo.lock`, `data/config.exemplo`.

---

### Tarefa 4: [sonnet] (Condicional) greeter — `SetLockedHint` no bloqueio

**Só com aprovação explícita do usuário.** Fato apurado: nenhum componente do COSMIC 1.10 (cosmic-comp, cosmic-session) nem o `cosmic-greeter-aenes` chama `SetLockedHint`, então o `LockedHint` que o design §2.11 usa para detectar o bloqueio fica sempre `false` (confirmado ao vivo no spike do daemon, chave `locked_hint_durante_bloqueio`). Sem esta tarefa, os cartões ficam **invisíveis** com a tela bloqueada (o compositor esconde layer surfaces não marcadas) — seguro, mas sem o "aviso sem conteúdo" do design. Com ela, o daemon marca as superfícies com `set_show_on_lock` e mostra a versão redigida.

**Files:**
- Modify: `~/Projetos/cosmic-greeter/src/logind.rs` (função nova), `~/Projetos/cosmic-greeter/src/locker.rs` (chamadas em `SessionLockEvent::Locked` e `::Unlocked`), `~/Projetos/cosmic-greeter/packaging/arch/PKGBUILD` (`pkgrel=3`)

**Interfaces:**
- Consumes: `logind_zbus::session::SessionProxy` (já dependência), `SessionLockEvent::{Locked, Unlocked}` (`locker.rs:1317`, `:1453`).
- Produces:
  ```rust
  // logind.rs
  /// Sessão de quem chama ("/org/freedesktop/login1/session/auto"); erro só vai para o log.
  pub async fn set_locked_hint(locked: bool) -> zbus::Result<()>;
  // locker.rs
  fn locked_hint_for(event: &SessionLockEvent) -> Option<bool>;  // Locked → Some(true), Unlocked → Some(false), Focused → None
  ```
- Chamada como `Task::future` que não bloqueia a interface; falha → `tracing::warn!`.

- [ ] **Passo 1: Teste que falha:** `#[test] fn locked_hint_follows_lock_events()` em `locker.rs`.
- [ ] **Passo 2:** `H 'cd ~/Projetos/cosmic-greeter && cargo test --lib locked_hint'` → FAIL. **Passo 3:** implementar. **Passo 4:** `H 'cd ~/Projetos/cosmic-greeter && cargo test --lib'` → PASS.
- [ ] **Passo 5:** `H 'cd ~/Projetos/cosmic-greeter/packaging/arch && makepkg -f'`; o usuário instala com `sudo pacman -U cosmic-greeter-aenes-1.11.0-3-x86_64.pkg.tar.zst` (volta: o `-2` na mesma pasta, como diz o cabeçalho do PKGBUILD).
- [ ] **Passo 6 (ao vivo, com o usuário):** `H '(sleep 20; busctl --system get-property org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session LockedHint > ~/Projetos/notificacoes/docs/verificacao/i4-lockedhint.txt; notify-send -a Telegram Fulano segredo; notify-send -a Teste "Pendrive"; ) & loginctl lock-session'` → arquivo com `b true`; sobre a tela de bloqueio aparece "Telegram · Nova notificação" sem o texto. Depois de desbloquear: `LockedHint` `false` e os avisos com conteúdo.
- [ ] **Passo 7: Deixar no working tree (não commitar)** — `src/logind.rs`, `src/locker.rs`, `packaging/arch/PKGBUILD`.

---

### Tarefa 5: [sonnet] Aceite integrado — roteiro fechado

Com o usuário, com o pacote do daemon instalado (T17 do daemon) e as T1–T3 (e T4, se feita) instaladas.

**Files:** `~/Projetos/notificacoes/docs/verificacao/i5-*.png` (prints). Nenhum código.

- [ ] **Passo 1 (print):** `Win+Shift+S` → aviso de imagem; "Editar" abre o Satty; pasta abre a pasta. A captura não é `transient`: conferir que ela está no histórico da Central com a ação "Editar" e que acioná-la pela Central depois de expirada abre o Satty (o `notify-send --wait` ainda está esperando, dentro do prazo de 10 min).
- [ ] **Passo 2 (logi-osd):** Caps, luz, Easy-Switch → pílulas, nenhuma no histórico; bateria baixa simulada (`--demo` do logi-osd) → cartões laranja e vermelho no canto **e** no histórico.
- [ ] **Passo 3 (Central):** limpar o histórico; 3 avisos com ações; deixar expirar; acionar cada um pela Central → `dbus-monitor` mostra `ActionInvoked` com o id e a chave certos; "não perturbe" pela Central → nada aparece, tudo no histórico.
- [ ] **Passo 4 (volta ao oficial, ensaio):** seguir o roteiro do PKGBUILD do daemon (`pacman -U` do `cosmic-notifications-1:1.10.0-1.1-x86_64_v3` + `pkill -x cosmic-notifications`), confirmar que o oficial volta e que logi-osd, print-recorte e Central continuam funcionando com ele (pílulas viram avisos comuns do oficial — esperado), depois reinstalar o pacote novo. Só com o usuário de acordo.
- [ ] **Passo 5: Relatório** ao usuário com prints e latência medida; deixar no working tree (não commitar) — só `docs/verificacao/i5-*`.

---

## Autorrevisão (feita)

- **Cobertura do design §3:** 3.1 → T2 (ajudante = `notify-send --wait -t`, aposentadoria do `print-preview` sem apagá-lo); 3.2 → T3 (sai `osd/`, `x-estilo osd`, `value`/`x-pontos`, `transient`, `synchronous`, bateria como cartão com `x-tom`, regras do spec do logi-osd mantidas); 3.3 → T1 (`transient`, `InvokeAction` pelo barramento). §2.11 (bloqueio) → T4 condicional, motivada por fato apurado.
- **Tipos consistentes com o plano do daemon:** nomes de dicas, valores de `x-tom`/`x-estilo`, caminho/interface/assinatura do `InvokeAction`, motivos de `NotificationClosed`, `--sem-tela`, `PrivateBus`, `NOTIFICACOES_DEBUG`.
- **Proporção:** só tabelas de mapeamento, flags exatas do `notify-send` e regras que os testes não fixam.

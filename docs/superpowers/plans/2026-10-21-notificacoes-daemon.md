# notificacoes (daemon) — Plano de Implementação

> **Para agentes:** SUB-SKILL OBRIGATÓRIA: use `superpowers:subagent-driven-development` (recomendado) ou `superpowers:executing-plans` para executar este plano tarefa a tarefa. Os passos usam checkbox (`- [ ]`).

**Objetivo:** daemon de notificações em Rust que substitui o `cosmic-notifications` do COSMIC 1.10 (mesmo nome de binário, mesmo nome no D-Bus), com o visual do `print-preview`, uma thread, sem toolkit e sem acordar quando não há aviso na tela.

**Arquitetura:** uma thread com um `calloop::EventLoop` que junta: a conexão Wayland (layer-shell, uma superfície por cartão e uma para a pílula, ponteiro), o fd do barramento de sessão (libdbus via crate `dbus`, servidor `org.freedesktop.Notifications` + `com.system76.NotificationsApplet`), o fd do barramento de sistema (logind), o monitor udev (USB), um inotify (não perturbe e config) e no máximo um timer, que só existe com aviso na tela ou grupo USB pendente. Toda a semântica do protocolo (ids, pilha, lista oculta, sinais) fica num `Store` puro; desenho em `tiny-skia` + `cosmic-text`, testado em `Pixmap`.

**Tech stack:** Rust 1.99, edition 2024. `smithay-client-toolkit 0.20.0`, `cosmic-protocols 0.2.0` (client), `wayland-scanner 0.31` (protocolo de bloqueio vendorizado), `tiny-skia 0.11.4`, `cosmic-text 0.19.0`, `resvg 0.45.1`, `png 0.18.1`, `zune-jpeg 0.5.15`, `dbus 0.9.12` (libdbus 1.16.2 do sistema), `udev 0.9.3`, `rustix 1`, `libc 0.2`. Justificativas na seção "Dependências".

**Spec:** `docs/superpowers/specs/2026-10-21-notificacoes-design.md` (fonte da verdade). Referência visual: `~/.local/bin/print-preview` (seção `CSS`). Base de código: `~/Projetos/logi-osd`. Plano irmão (mudanças fora do daemon): `docs/superpowers/plans/2026-10-21-notificacoes-integracoes.md`, que consome as Interfaces deste plano.

## Pré-requisitos (feitos pelo USUÁRIO, não pelo agente)

1. `cd ~/Projetos/notificacoes && git init -b main && git checkout -b develop` — o repositório ainda não é git. O controller **pergunta a branch** (feature/ de develop, hotfix/ de main, direto em develop ou main) antes da Tarefa 1, conforme o CLAUDE.md global.
2. Toda execução de `cargo`, `grim`, `busctl`, `dbus-monitor`, `notify-send` e do daemon é no host. Abaixo, `H '<cmd>'` abrevia exatamente `flatpak-spawn --host sh -c 'cd ~/Projetos/notificacoes && <cmd>'`.
3. A Tarefa 1 e as verificações ao vivo exigem o usuário na frente da máquina (bloquear a tela, olhar o painel, clicar). O agente prepara os comandos; o usuário executa ou acompanha.

## Regras de execução (toda tarefa herda)

- **Ninguém commita.** Todo passo final é "Deixar as mudanças no working tree (não commitar)" com a lista de arquivos. Todo prompt de implementer leva "não commite".
- **Tier do implementer** marcado no título de cada tarefa (`[haiku]` mecânico, `[sonnet]` padrão, `[opus]` integração/concorrência/risco). Revisor é sempre `fable`.
- **Licença de corrigir o plano:** se o código do brief não compilar, contradisser as assinaturas reais (sctk, dbus, cosmic-text, tiny-skia, resvg, udev, rustix) ou falhar nos testes do próprio brief, o plano pode estar errado — corrija para satisfazer o teste e relate a divergência; nunca altere o teste para acomodar implementação defeituosa.
- **Resultados do spike mandam.** Onde um passo diz "conforme `resultados-spike.txt: <chave>`", o valor está em `docs/verificacao/resultados-spike.txt` (Tarefa 1). Chave ausente ou `inconclusivo`: **pare e relate**.
- TDD em toda lógica testável. Partes Wayland/udev ao vivo viram passo de verificação manual com comando e resultado esperado; prints com `grim` salvos em `docs/verificacao/` (nunca `/tmp`).

## Global Constraints

- Pacote cargo `notificacoes`, binário `notificacoes`; instalado como `/usr/bin/cosmic-notifications` só pelo PKGBUILD (Tarefa 17). Licença `GPL-3.0-only` (copiar `~/Projetos/logi-osd/LICENSE`), `edition = "2024"`, `rust-version = "1.88"` (subir se uma dependência exigir). `[profile.release]` igual ao logi-osd: `opt-level = 3`, `panic = "abort"`, `lto = "thin"`, `codegen-units = 1`, `strip = true`.
- **Proibido:** libcosmic, iced, GTK, zbus, tokio/qualquer runtime async, serde, tracing, `freedesktop-icons`, `image`. Logs com `eprintln!("notificacoes: …")` (a sessão manda o stderr ao journal).
- **Uma thread** (`Threads: 1` em `/proc/PID/status`). Nenhum `std::thread::spawn`.
- **Zero acordadas parado:** sem polling; timer calloop só existe enquanto houver cartão/pílula na tela, animação, grupo USB pendente ou pedido de token de ativação pendente. A inscrição de toplevels do compositor só existe com cartão de app visível (Tarefa 13).
- **Nunca sair por erro recuperável.** Morte do daemon faz o `cosmic-session` reiniciar **o daemon e o painel** (`cosmic-session/src/notifications.rs`, `with_on_exit`). Só sai (código 1) se a conexão Wayland falhar ou cair. Falha de D-Bus, logind, udev, inotify, fonte, ícone ou imagem: loga e segue degradado. Nenhum `unwrap`/`expect`/indexação sem checagem sobre dado externo (D-Bus, arquivos, udev).
- **Nome no barramento:** `RequestName("org.freedesktop.Notifications")` com `allow_replacement = false`, `replace_existing = true`, `do_not_queue = false`. Fato apurado: o oficial (zbus 5 `Builder::name`) pede o nome com `AllowReplacement | ReplaceExisting | DoNotQueue`, então o daemon novo o toma sem matar o oficial, e um oficial reiniciado não o toma de volta (falha 5 vezes e fica ocioso). Confirmado ao vivo na Tarefa 1.
- **Objetos D-Bus:** `/org/freedesktop/Notifications` interface `org.freedesktop.Notifications` (spec 1.2: `Notify`, `CloseNotification`, `GetCapabilities`, `GetServerInformation`; sinais `NotificationClosed(u id, u reason)`, `ActionInvoked(u id, s key)`, `ActivationToken(u id, s token)`); `/com/system76/NotificationsApplet` interface `com.system76.NotificationsApplet` método `InvokeAction(u id, s action)`; `org.freedesktop.DBus.Introspectable.Introspect` e `org.freedesktop.DBus.Peer.Ping` nos dois caminhos. `GetCapabilities` = `["actions","action-icons","body","body-markup","icon-static","persistence","sound"]`. `GetServerInformation` = `("notificacoes", "Aenes", env!("CARGO_PKG_VERSION"), "1.2")`. Responde a `Notify` sempre, exceto quando a mensagem tem `NO_REPLY_EXPECTED`.
- **Fechamento (design §8.1, conferido no upstream 1.10):** expirar ou sair da pilha **não** emite sinal; a notificação vai para a lista oculta (máx. **200**, mais antiga sai primeiro, sem dados de imagem). `NotificationClosed` só em: × → motivo 2; clique/ação → `ActivationToken` (se houver token) + `ActionInvoked` + `NotificationClosed` motivo 2; `CloseNotification` → motivo 3. Avisos `transient` (inclui toda pílula) emitem motivo 1 ao expirar e não entram na lista oculta.
- `DAEMON_NOTIFICATIONS_FD`: se presente e for socket, marcar `FD_CLOEXEC` e **segurar aberto sem servir** (o painel só conecta quando há applet `X-NotificationsApplet`, com prazo de 1 s, e só registra erro; hoje não há nenhum). A Central aciona ações por `InvokeAction` no barramento de sessão.
- **Dicas aceitas** (nomes exatos): `image-data`/`image_data`/`icon_data` (`(iiibiiay)`), `image-path`/`image_path` (caminho, `file://` URI com `%XX`, ou nome de ícone), `urgency` (byte 0/1/2), `category`, `desktop-entry`, `transient`, `sound-name`, `sound-file`, `suppress-sound`, `x-canonical-private-synchronous`/`synchronous` (string), `action-icons`, `value` (int 0–100), e as nossas: `x-estilo` = `osd`\|`cartao`\|`imagem`\|`compacto`, `x-tom` = `normal`\|`atencao`\|`critico`\|`apagado`, `x-pontos` (int), `x-pontos-total` (int). Booleanos aceitam `b` ou inteiro ≠ 0. Dica com tipo errado é ignorada.
- `x-estilo=compacto` é extensão interna (design §2.2, "um tipo novo de aviso é um estilo novo"): usada pelo próprio daemon para os avisos de USB, que são enviados **como chamada `Notify` ao próprio nome** para a Central (que só vê chamadas `Notify` no barramento) registrá-los no histórico.
- **Nível efetivo** (cor e tempo): `x-tom` se for `normal`/`atencao`/`critico`; senão `urgency` 2 → crítico, 0/1 → normal. `apagado` só afeta o valor da pílula (nível normal).

| Nível | Cor dos detalhes (barra, botão principal, destaque do ícone) | Tempo |
|---|---|---|
| normal | destaque do tema COSMIC | 6000 ms |
| atenção | `#F0A35E` (`WARNING` do logi-osd) | 10000 ms |
| crítico | `#E5534B` (`CRITICAL` do logi-osd) | 15000 ms |

- `expire_timeout` respeitado só se `0 < t < tempo do nível`; `0`, `-1` e maiores valem o tempo do nível. Nada fica preso.
- **Visual (lógico, × escala inteira da superfície):** fundo modo escuro `#1e1f24` a 94%; borda branca a 8%; raio 16; padding 10; sombra gaussiana σ = 14, deslocada 8 para baixo, preta a 45%, cortada no enchimento da superfície (esq 20, topo 12, dir 20, base 24); fade de entrada 180 ms ease-out cúbica; título 13 px peso 600 `#f2f2f5`; linha "App · agora" 11 px a 60%; corpo 12 px a 85%, até 3 linhas com "…"; botões em pílula altura 26, padding 14, texto 13 px peso 600, principal na cor do nível com texto `#0b1a24` (ou `#f2f2f5` se a cor for escura, luminância < 0,4), secundários branco a 6% (hover 14%); botão redondo 30 (branco 6%, hover 14%); barra 3 px, raio 2, trilho branco 8%, progresso na cor do nível, 8 abaixo do conteúdo. Modo claro: fundo = `background.base` do tema claro a 94%, texto = `background.on`, borda e botões em preto a 8%/6%/14%. Largura do cartão e da imagem grande **340**; miniatura até **320×180**, raio 10, fundo preto 35%. Margem da pilha **24** da base e da direita; **8** entre cartões; mais nova embaixo; máximo **4** visíveis.
- Camada `Overlay`, `KeyboardInteractivity::None`, `exclusive_zone = 0`, namespace `notificacoes`; região de entrada = retângulo do cartão (pílula: região vazia). Sem saída fixa no primeiro cartão (o cosmic-comp escolhe a ativa); os seguintes vão para a saída onde o primeiro entrou, enquanto a pilha existir.
- **Textos pt-BR fixos:** `agora`; `Nova notificação` (modo privado); USB: `Controle`, `Armazenamento USB`, `Celular`, `Dispositivo de áudio`, `Teclado`, `Mouse`, `Dispositivo USB` + ` conectado`/` desconectado`; grupo `{n} dispositivos conectados`/`desconectados`; nome de app vazio → `Notificação`; app dos avisos USB `Dispositivos USB`.
- **Config** opcional `$XDG_CONFIG_HOME/notificacoes/config` (senão `~/.config/notificacoes/config`), `chave = valor`, `#` comenta, linha inválida mantém o padrão e avisa com o nº da linha (mesmo estilo do logi-osd): `duracao_normal_ms = 6000`, `duracao_atencao_ms = 10000`, `duracao_critico_ms = 15000` (faixa 500..=120000), `max_visiveis = 4` (1..=8), `som = sim`, `som_padrao = message` (vazio = sem som padrão), `som.<categoria> = <nome>|nao` (padrões: `device.added → device-added`, `device.removed → device-removed`, `im.received → message-new-instant`, `email.arrived → message-new-email`), `tocador = pw-play`, `usb = sim`, `usb_avisa = controle armazenamento celular audio teclado mouse generico`, `fonte =` (vazio = fonte da interface do COSMIC, senão `Open Sans`).
- Não perturbe: `~/.config/cosmic/com.system76.CosmicNotifications/v1/do_not_disturb` (`true`/`false`, ausente = `false`). Ligado: aceita, dá id, nada aparece (nem pílula), nada toca; cartões visíveis somem para a lista oculta sem sinal.
- Medição (método do usuário): delta de `voluntary_ctxt_switches + nonvoluntary_ctxt_switches` somado em `/proc/PID/task/*/status` em 30 s parado, `VmRSS`, `Threads`. Referência 2026-10-21: oficial 18,5 MB / 35 threads / 16 acordadas; logi-osd 4,2 MB / 3 / 0. **Meta: `Threads: 1`, 0 trocas de contexto em 30 s parado, `VmRSS` ≤ 12 MB.**

### Dependências (pesquisadas; uma linha de motivo cada)

| Crate | Uso | Por que esta |
|---|---|---|
| `dbus 0.9.12` (`default-features = false`) | servidor de sessão + cliente de sistema | libdbus 1.16.2 já está no sistema; `Channel::watch()` dá o fd para o calloop → uma thread e zero acordadas. `zbus 5.19` cria a thread `zbus::Connection executor` por conexão (`connection/builder.rs:789`) mais a do reator async-io; `rustbus 0.19.3` serviria mas é menos usado e também exige integração de fd própria. |
| `smithay-client-toolkit 0.20.0` (`default-features = false`, `features = ["calloop"]`) | layer-shell, seat/ponteiro, `cursor_shape`, `activation`, `foreign_toplevel_list` | já usado no logi-osd; sem `xkbcommon` (não há teclado). |
| `cosmic-protocols 0.2.0` (`default-features = false`, `features = ["client"]`) | `zcosmic_toplevel_info_v1` v2/v3 (`get_cosmic_toplevel`, estado `fullscreen`/`activated`) | é o protocolo que o cosmic-comp 1.10 expõe a clientes fora de sandbox. |
| `protocols/cosmic-session-lock-layer-v1.xml` + `wayland-scanner 0.31` | `cosmic_session_lock_layer_manager_v1.set_show_on_lock` | o cosmic-comp 1.10 esconde **toda** layer surface com a sessão bloqueada, exceto as marcadas por este protocolo (`shell/focus/order.rs`, `layer_show_on_lock`); ele só existe no `main` do cosmic-protocols (commit `c0cff4d`, 2026-09-11), não no 0.2.0 do crates.io → vendorizar o XML (licença MIT, copiar o cabeçalho). |
| `tiny-skia 0.11.4` | desenho | já usado; é o mesmo que o `resvg 0.45` usa. |
| `cosmic-text 0.19.0` (`default-features = false`, `features = ["std","swash","fontconfig"]`) | texto | fallback de fonte por script (fontdb + fontconfig), shaping (harfrust) para árabe/índico, emoji colorido CBDT (Noto Color Emoji) via swash, e `Ellipsize::End(Lines(3))` pronto. `ab_glyph` (logi-osd) não tem fallback, nem shaping, nem emoji colorido. **Custo:** índice de ~821 arquivos de fonte deste sistema na RAM + caches; medido no spike (Tarefa 1), com "modo reduzido" previsto (Tarefa 6). |
| `resvg 0.45.1` (`default-features = false`) | ícones SVG do tema (Cosmic/Pop/Adwaita são quase só SVG) | usa o mesmo `tiny-skia 0.11`; sem `text`/fontes do sistema. |
| `png 0.18.1`, `zune-jpeg 0.5.15` (`default-features = false`, `features = ["std","x86"]`) | imagem grande e ícones PNG | decodificadores puros e pequenos; `image` puxa codecs que não usamos. |
| `udev 0.9.3` | USB | já usado no logi-osd. |
| `rustix 1` (`features = ["event","fs","process"]`) | inotify, `pidfd_open`, poll | já usado; `pidfd` evita thread para colher o `pw-play`. |
| `libc 0.2` | `malloc_trim(0)` depois de imagem grande e ao esvaziar a tela | devolve ao sistema o heap de decodificação. |
| `pw-play` (processo filho, sob demanda) | som | `canberra-gtk-play`/libcanberra trazem GStreamer/Pulse e threads; filho só existe enquanto toca. |
| ícones: código próprio (~150 linhas, Tarefa 7) | tema de ícones + `.desktop` | `freedesktop-icons 0.4.0` guarda raízes e temas em `static Lazy` lido do ambiente → impossível testar com fixtures e mantém o índice de todos os temas na RAM. |

## Review Focus

1. **Texto de mensageiro (WhatsApp/Telegram via Brave): emoji, árabe, CJK, markup quebrado** → glifos certos, emoji colorido, 3 linhas com "…", nada de quadradinho, nenhum pânico — e o custo de RAM da escolha `cosmic-text` (meta ≤ 12 MB parado). Testes: `emoji_renders_color_pixels`, `cjk_and_arabic_render_ink`, `lone_less_than_is_kept` (T6/T2); medição na T18.
2. **Imagem hostil ou enorme** (`image-data` com `rowstride` mentiroso, PNG 20000×20000, arquivo sumido, `file://` com `%20`, print 4K) → nunca pânico, cai para cartão comum, RSS volta ao normal depois. Testes: `raw_image_with_short_buffer_is_rejected`, `png_over_pixel_limit_rejected_before_decode`, `file_uri_percent_decoded` (T11/T2); RSS pós-print na T18.
3. **Rajada** (50 avisos em 1 s ao abrir um grupo) → 4 visíveis, ids únicos, nenhum perdido, no máximo um `pw-play` por vez. Testes: `burst_keeps_four_visible_and_all_ids_unique` (T4), `sound_skipped_while_previous_playing` (T15).
4. **Briga pelo nome / reinício** (oficial religado pela sessão, daemon reiniciado) → sem loop de crash, sem perder o nome, sem tomar o nome de quem não permite. Testes: `takes_name_from_owner_that_allows_replacement`, `waits_in_queue_when_owner_forbids_replacement` (T5).
5. **Ação acionada pela Central de notificação já expirada, fechada ou com id desconhecido** → `ActionInvoked` certo ou nada; nunca a ação de outra notificação. Testes: `expired_card_emits_nothing_and_stays_invokable`, `invoke_unknown_id_is_silent`, `invoke_unknown_action_is_ignored` (T4/T5).

---

## Estrutura de arquivos

```
notificacoes/
├── Cargo.toml, LICENSE, .gitignore            # T2
├── protocols/cosmic-session-lock-layer-v1.xml # T13 (vendorizado, MIT)
├── data/config.exemplo                        # T3
├── packaging/arch/PKGBUILD                    # T17
├── tools/spike/                               # T1 (crate próprio, descartável, não instalado)
├── tools/dev.sh                               # T9 (modo de desenvolvimento)
├── docs/verificacao/                          # T1+ (resultados-spike.txt, prints .png)
├── tests/
│   ├── common/mod.rs                          # T5 PrivateBus (dbus-daemon privado)
│   ├── dbus.rs                                # T5
│   ├── logind.rs                              # T13
│   └── fixtures/{icons,applications,images,sounds,theme}/
└── src/
    ├── main.rs        # T5 (args, State, funil de efeitos), T9 (Wayland), T12–T16 (fontes do loop)
    ├── model.rs       # T2  Notification, Hints, Action, níveis (pura)
    ├── markup.rs      # T2  texto puro (pura)
    ├── config.rs      # T3
    ├── timing.rs      # T3  tempos, Countdown, passo da barra, fade (pura)
    ├── store.rs       # T4  ids, pilha, lista oculta, efeitos (pura)
    ├── panel.rs       # T5  DAEMON_NOTIFICATIONS_FD
    ├── headless.rs    # T5  modo --sem-tela (só timers de expiração)
    ├── dbus/{mod,server,conn,logind}.rs   # T5 (server/conn), T13 (logind)
    ├── render/{mod,theme,text,shapes,card,pill,glyphs}.rs   # T2 theme, T6 text, T8 card/shapes, T16 pill/glyphs
    ├── icons.rs       # T7
    ├── images.rs      # T11
    ├── presenter.rs   # T9/T10 lógica de pilha, wake, hit-test (pura)
    ├── wayland/{mod,surface,pointer,toplevel,lock_layer}.rs # T9, T10, T13
    ├── watch.rs       # T12 inotify
    ├── privacy.rs     # T13 (pura)
    ├── usb/{mod,classify,group}.rs  # T14
    └── sound.rs       # T15
```

## Tarefas

1. [opus] Spike de risco — verificação ao vivo, sem produto
2. [sonnet] Esqueleto, modelo e dicas — parsing fechado
3. [sonnet] Configuração e regras de tempo — pausa e passo
4. [opus] Store do protocolo — estados e sinais
5. [opus] Servidor D-Bus no calloop — libdbus, nome, barramento privado
6. [sonnet] Texto — fallback, emoji, reticências
7. [sonnet] Ícones de app — tema, .desktop, SVG
8. [sonnet] Desenho do cartão — geometria e pixels
9. [opus] Superfícies Wayland e pilha — várias superfícies, timer único
10. [opus] Ponteiro e ações — hover, clique, ativação
11. [sonnet] Imagem grande — decodificação com limites
12. [sonnet] Não perturbe e recarga — inotify simples
13. [opus] Privacidade: bloqueio e tela cheia — dois barramentos, protocolos
14. [opus] USB embutido — udev, agrupamento, suspensão
15. [sonnet] Som — tema e pidfd
16. [sonnet] Pílula OSD — porte do logi-osd
17. [haiku] Pacote Arch — molde do greeter
18. [sonnet] Medição e aceite ao vivo — roteiro fechado
19. [opus] (Opcional) Compartilhamento de tela — investigação com portão

Ordem do design §7 respeitada: núcleo (T2–T10, T12) → imagem (T11, libera o print-recorte do plano irmão) → USB + som (T14, T15) → pílula (T16, libera o logi-osd) → privacidade (T13 pode ir antes de T14) → pacote/aceite → opcional.

---

### Tarefa 1: [opus] Spike de risco — verificação ao vivo, sem produto

Código descartável num crate próprio, fora do daemon. Termina num **portão**: o controller mostra `resultados-spike.txt` ao usuário e só segue para T6, T9, T13 e T17 depois da aprovação.

**Files:**
- Create: `tools/spike/Cargo.toml` (com `[workspace]` vazio, para não entrar no crate principal), `tools/spike/src/bin/camada.rs`, `tools/spike/src/bin/nome.rs`, `tools/spike/src/bin/texto.rs`, `tools/spike/protocols/cosmic-session-lock-layer-v1.xml` (cópia do `main` do cosmic-protocols: `gh api "repos/pop-os/cosmic-protocols/contents/unstable/cosmic-session-lock-layer-v1.xml?ref=main" --jq .content | base64 -d`)
- Create (gerados): `docs/verificacao/resultados-spike.txt`, `docs/verificacao/spike-*.png`

**Interfaces:**
- Produces: `docs/verificacao/resultados-spike.txt`, uma `chave = valor` por linha, com exatamente as chaves abaixo (valores `sim`/`nao`/número/`inconclusivo`).
- `camada [--top] [--no-bloqueio] [--segundos N]`: layer surface 300×120 magenta `#FF00FF` opaco, ancorada embaixo à direita, margem 24, camada Overlay (ou Top), `--no-bloqueio` chama `set_show_on_lock`; some após N s (padrão 20).
- `nome`: pede `org.freedesktop.Notifications` com as flags das Global Constraints, imprime `PrimaryOwner`/`InQueue`, responde `Notify` com ids e imprime cada chamada.
- `texto`: imprime `VmRSS` antes e depois de `FontSystem::new()`, o tempo da criação, e depois de compor e rasterizar `"Olá 👋 مرحبا 你好 שלום नमस्ते"` a 12 px; grava `docs/verificacao/spike-texto.png`.

- [ ] **Passo 1: Escrever os três binários.** Sem testes (spike). `H 'cd tools/spike && cargo build --release'` → compila.
- [ ] **Passo 2 (b — print):** `H 'tools/spike/target/release/camada --segundos 15 & sleep 2; grim docs/verificacao/spike-grim-overlay.png'`; repetir com `--top` → `spike-grim-top.png`. Abrir os PNGs (ferramenta Read) e procurar o retângulo magenta. Repetir com o `cosmic-screenshot` (atalho Print do usuário) e copiar o arquivo para `docs/verificacao/spike-cosmic-screenshot.png`. Chaves: `grim_captura_overlay`, `grim_captura_top`, `cosmic_screenshot_captura_overlay`.
- [ ] **Passo 3 (a — bloqueio, com o usuário):** `H 'tools/spike/target/release/camada --segundos 60 & (sleep 25; busctl --system get-property org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session LockedHint > docs/verificacao/spike-lockedhint.txt) & loginctl lock-session'`. O usuário diz se viu o magenta sobre a tela de bloqueio. Repetir com `--no-bloqueio`. Chaves: `bloqueio_overlay_sem_protocolo` (esperado `invisivel`, pelo código do cosmic-comp), `bloqueio_overlay_com_show_on_lock` (esperado `visivel`), `locked_hint_durante_bloqueio` (esperado `nao`: nenhum componente do COSMIC 1.10 nem o `cosmic-greeter-aenes` chama `SetLockedHint`).
- [ ] **Passo 4 (c — nome e FD, com o usuário olhando o painel):**
  1. `H 'pid=$(pidof cosmic-notifications); tr "\0" "\n" < /proc/$pid/environ | grep DAEMON_NOTIFICATIONS_FD; ls -l /proc/$pid/fd/$(tr "\0" "\n" < /proc/$pid/environ | sed -n "s/^DAEMON_NOTIFICATIONS_FD=//p")'` → `daemon_fd_tipo = socket`.
  2. `H 'tools/spike/target/release/nome > docs/verificacao/spike-nome.log 2>&1 & sleep 1; busctl --user status org.freedesktop.Notifications | grep -E "^PID|^Comm"; notify-send spike ok; pidof cosmic-notifications'` → `nome_tomado_do_oficial` (PID do `nome`), `oficial_continua_vivo`.
  3. `H 'pkill -x nome; sleep 1; busctl --user status org.freedesktop.Notifications'` (esperado: sem dono) e depois `H 'pkill -x cosmic-notifications; sleep 3; busctl --user status org.freedesktop.Notifications | grep -E "^PID|^Comm"; pidof cosmic-panel'` → `oficial_religado_pega_nome`, `painel_reiniciou_ao_matar_oficial` (o PID do `cosmic-panel` mudou; o usuário confirma o piscar do painel).
- [ ] **Passo 5 (d — texto):** `H 'tools/spike/target/release/texto'` → chaves `cosmic_text_rss_kb_antes`, `cosmic_text_rss_kb_depois`, `cosmic_text_ms_fontsystem`, `cosmic_text_emoji_colorido` (olhar `spike-texto.png`).
- [ ] **Passo 6: Portão.** Apresentar `resultados-spike.txt` ao usuário com o que muda em cada resultado negativo:

| Resultado | Consequência no plano |
|---|---|
| `grim_captura_overlay = nao` e `grim_captura_top = sim` | Global Constraints: camada `Top` para cartões e pílula. |
| ambos `nao` | **Parar**: requisito §2.10 do design não atendível; relatar. |
| `bloqueio_overlay_com_show_on_lock = invisivel` | T13 sem `lock_layer`: com a tela bloqueada nada aparece; avisar o usuário (assunto do fork do greeter). |
| `locked_hint_durante_bloqueio = nao` | Esperado. A privacidade no bloqueio depende da tarefa do greeter (plano irmão, T4); sem ela os cartões ficam invisíveis no bloqueio (seguro). |
| `nome_tomado_do_oficial = nao` | `tools/dev.sh iniciar` passa a fazer `pkill -x cosmic-notifications` depois de iniciar o daemon (que espera na fila); o painel reinicia a cada sessão de teste. |
| `cosmic_text_rss_kb_depois − antes > 8192` ou `cosmic_text_ms_fontsystem > 500` | T6 usa o **modo reduzido** de fontes. |

- [ ] **Passo 7: Deixar no working tree (não commitar)** — `tools/spike/`, `docs/verificacao/`.

---

### Tarefa 2: [sonnet] Esqueleto, modelo e dicas — parsing fechado

**Files:**
- Create: `Cargo.toml` (só `tiny-skia` e `libc` por ora; cada tarefa acrescenta as suas), `LICENSE`, `.gitignore` (`/target`, `/tools/spike/target`), `src/main.rs` (mínimo: `mod`s e `fn main` vazio que retorna `ExitCode::SUCCESS`), `src/model.rs`, `src/markup.rs`, `src/render/mod.rs`, `src/render/theme.rs` (cópia de `~/Projetos/logi-osd/src/osd/theme.rs` com seus testes e de `tests/fixtures/theme/*`)
- Test: `#[cfg(test)]` em cada arquivo

**Interfaces:**
- Produces (`model.rs`):
  ```rust
  #[derive(Debug, Clone, PartialEq)] pub enum HintValue { Bool(bool), Int(i64), Str(String), Image(RawImage) }
  #[derive(Debug, Clone, PartialEq)] pub struct RawImage { pub width: i32, pub height: i32, pub rowstride: i32, pub has_alpha: bool, pub bits_per_sample: i32, pub channels: i32, pub data: Vec<u8> }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)] pub enum Urgency { Low, #[default] Normal, Critical }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Level { Normal, Atencao, Critico }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Tom { Normal, Atencao, Critico, Apagado }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Estilo { Osd, Cartao, Imagem, Compacto }
  #[derive(Debug, Clone, PartialEq)] pub enum ImageHint { Data(RawImage), File(PathBuf), IconName(String) }
  #[derive(Debug, Clone, PartialEq, Default)] pub struct Hints {
      pub urgency: Urgency, pub category: Option<String>, pub desktop_entry: Option<String>, pub transient: bool,
      pub image: Option<ImageHint>, pub value: Option<u8>, pub sound_name: Option<String>, pub sound_file: Option<PathBuf>,
      pub suppress_sound: bool, pub sync_key: Option<String>, pub estilo: Option<Estilo>, pub tom: Option<Tom>,
      pub pontos: Option<(u8, u8)>, pub action_icons: bool,
  }
  impl Hints { pub fn parse(raw: Vec<(String, HintValue)>) -> Hints }
  #[derive(Debug, Clone, PartialEq, Eq)] pub struct Action { pub key: String, pub label: String }
  pub fn parse_actions(flat: Vec<String>) -> Vec<Action>;
  #[derive(Debug, Clone, PartialEq)] pub struct Notification {
      pub id: u32, pub app_name: String, pub app_icon: String, pub summary: String,
      pub body: String /* já em texto puro */, pub actions: Vec<Action>, pub hints: Hints, pub expire_timeout: i32,
  }
  impl Notification {
      pub fn level(&self) -> Level;        // regra "Nível efetivo" das Global Constraints
      pub fn is_pill(&self) -> bool;       // estilo == Some(Osd)
      pub fn is_transient(&self) -> bool;  // hints.transient || is_pill()
      pub fn is_private(&self) -> bool;    // false se pílula, Compacto ou category começa com "device"
      pub fn display_app(&self) -> &str;   // app_name aparado, ou "Notificação"
  }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum UsbKind { Controle, Armazenamento, Celular, Audio, Teclado, Mouse, Generico }
  ```
- Produces (`markup.rs`): `pub fn plain(text: &str) -> String` — tira tags (`<b>`, `<i>`, `<u>`, `<a …>`, `<img …/>`), decodifica `&lt; &gt; &quot; &apos; &#39; &amp;` e `&#NNN;`/`&#xHH;`, apara; um `<` sem `>` depois fica como texto.
- Produces (`render/theme.rs`): exatamente as do logi-osd (`Rgba`, `Theme`, `WARNING`, `CRITICAL`, `FALLBACK`, `parse_hex`, `top_level_field`, `latest_version_dir`, `load`).
- Regras de `Hints::parse`: prioridade de imagem `image-data` > `image_data` > `image-path` > `image_path` > `icon_data`; `image-path` que começa com `/` → `File`, com `file://` → `File` com `%XX` decodificado, outro texto não vazio → `IconName`; `urgency` aceita `Int` 0/1/2 (outro → `Normal`); `value` limitado a 0..=100; `pontos` só com `x-pontos` e `x-pontos-total` presentes, `total` limitado a 1..=20 e `on` a `total`; strings vazias = ausente.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn image_variants_and_priority()       // image-data vence image-path; icon_data só se nada mais; Data(RawImage) preservado
#[test] fn image_path_forms()                  // "/x/a.png"→File("/x/a.png"); "file:///x/a%20b.png"→File("/x/a b.png"); "dialog-information"→IconName
#[test] fn file_uri_percent_decoded()          // "file:///home/u/Imagens/Captura%C3%A7%20x.png" → "/home/u/Imagens/Capturaç x.png"
#[test] fn urgency_from_int_values()           // 0→Low, 1→Normal, 2→Critical, 7→Normal, Str→Normal
#[test] fn transient_accepts_bool_and_nonzero_int() // Bool(true)/Int(1)→true; Bool(false)/Int(0)/Str("true")→false
#[test] fn sync_key_from_both_names()          // x-canonical-private-synchronous e synchronous; vazio → None
#[test] fn estilo_and_tom_parse()              // "osd"/"cartao"/"imagem"/"compacto"; "apagado"; "azul" → None
#[test] fn pontos_requires_both_and_clamps()   // (5,3)→(3,3); só x-pontos → None; total 0 → None; total 99 → 20
#[test] fn value_clamped()                     // 150→100, -3→0
#[test] fn wrong_types_are_ignored()           // category=Int(3) → None; suppress-sound=Str → false
#[test] fn actions_in_pairs_odd_dropped()       // ["default","Abrir","x"] → [Action{default,Abrir}]
#[test] fn level_rules()                       // tom critico + urgency Low → Critico; sem tom + Critical → Critico; tom apagado → Normal; tom atencao → Atencao
#[test] fn private_rules()                     // pílula, compacto, category "device.added"/"device" → não privado; "im.received" → privado
#[test] fn empty_app_name_shows_notificacao()
#[test] fn markup_stripped_and_entities_decoded() // "Pronto: <a href=\"x\">site</a> &amp; <b>mais</b>" → "Pronto: site & mais"
#[test] fn numeric_entities_decoded()          // "&#233;&#xE9;" → "éé"
#[test] fn img_tag_removed()                   // "a<img src=\"x\"/>b" → "ab"
#[test] fn lone_less_than_is_kept()            // "2 < 3 e 5 > 4" → "2 < 3 e 5 > 4"
```
- [ ] **Passo 2:** `H 'cargo test'` → FAIL (símbolos inexistentes). **Passo 3:** implementar. **Passo 4:** `H 'cargo test'` → PASS (inclusive os testes copiados do tema).
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `Cargo.toml`, `LICENSE`, `.gitignore`, `src/main.rs`, `src/model.rs`, `src/markup.rs`, `src/render/{mod,theme}.rs`, `tests/fixtures/theme/*`.

---

### Tarefa 3: [sonnet] Configuração e regras de tempo — pausa e passo

**Files:**
- Create: `src/config.rs`, `src/timing.rs`, `data/config.exemplo` (todas as chaves com os padrões, comentadas em pt-BR)

**Interfaces:**
- Consumes: `Level`, `UsbKind` (T2).
- Produces:
  ```rust
  // timing.rs
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Durations { pub normal: Duration, pub atencao: Duration, pub critico: Duration }
  pub fn display_duration(level: Level, expire_timeout: i32, d: &Durations) -> Duration;
  #[derive(Debug, Clone)] pub struct Countdown { /* total, restante congelado, início da corrida atual */ }
  impl Countdown {
      pub fn start(total: Duration, now: Instant) -> Countdown;
      pub fn pause(&mut self, now: Instant); pub fn resume(&mut self, now: Instant);
      pub fn restart(&mut self, total: Duration, now: Instant);
      pub fn remaining(&self, now: Instant) -> Duration; pub fn fraction(&self, now: Instant) -> f32; // 1 → 0
      pub fn deadline(&self) -> Option<Instant>;  // None enquanto pausado
      pub fn is_paused(&self) -> bool;
  }
  pub const MIN_FRAME: Duration = Duration::from_millis(16);
  /// Instante em que a barra encolhe 1 px de dispositivo; nunca antes de now + MIN_FRAME; None se pausado ou acabou.
  pub fn next_bar_step(c: &Countdown, bar_device_px: u32, now: Instant) -> Option<Instant>;
  pub const FADE_IN: Duration = Duration::from_millis(180);
  pub fn fade_alpha(elapsed: Duration) -> f32;   // 1 - (1 - t)^3, t = elapsed/180ms limitado a 1
  // config.rs
  #[derive(Debug, Clone, PartialEq)] pub struct Config {
      pub durations: Durations, pub max_visible: usize, pub sound: bool, pub sound_default: Option<String>,
      pub sounds: Vec<(String, Option<String>)>, pub player: String, pub usb: bool, pub usb_kinds: Vec<UsbKind>, pub font: Option<String>,
  }
  impl Default for Config;  // valores das Global Constraints
  impl Config { pub fn sound_for_category(&self, category: Option<&str>) -> Option<&str>; } // som.<cat> (None se "nao"), senão som_padrao
  pub fn parse(text: &str) -> (Config, Vec<String>);
  pub fn load() -> Config;  pub fn config_path() -> PathBuf;  pub fn config_home() -> PathBuf; // como no logi-osd
  ```
- `usb_avisa` aceita os nomes `controle armazenamento celular audio teclado mouse generico` separados por espaço; nome desconhecido avisa e é ignorado; lista vazia = nenhum.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn defaults_match_design()          // 6000/10000/15000 ms; max 4; som sim; som_padrao Some("message"); 4 categorias; tocador "pw-play"; usb sim; 7 tipos
#[test] fn duration_range_warns()           // duracao_normal_ms = 100 → padrão + 1 aviso com "linha 1"
#[test] fn sound_category_off_is_none()     // "som.im.received = nao" → sound_for_category(Some("im.received")) == None
#[test] fn sound_default_fallback()         // categoria sem entrada → Some("message"); "som_padrao =" → None
#[test] fn max_visible_range()              // 0 e 9 → avisos, fica 4
#[test] fn usb_kinds_parse()                // "usb_avisa = controle mouse" → [Controle, Mouse]; "xyz" → aviso
#[test] fn config_path_like_logi_osd()      // XDG absoluto e fallback ~/.config, sufixo notificacoes/config
#[test] fn display_duration_rule()          // (Normal,3000)→3s; (Normal,9000)→6s; (Normal,0)→6s; (Normal,-1)→6s; (Critico,14000)→14s; (Atencao,20000)→10s
#[test] fn countdown_pause_freezes()        // start 6s; pause em +1s; remaining em +5s == 5s; deadline None
#[test] fn countdown_resume_continues()     // resume em +5s → deadline == +10s
#[test] fn countdown_restart()              // restart(10s, t) → remaining(t) == 10s, fraction 1.0
#[test] fn fraction_goes_one_to_zero()      // 0 → 1.0; metade → 0.5; depois do fim → 0.0
#[test] fn bar_step_scales_with_pixels()    // 6s, 320 px → ≈ now+18,75ms; 640 px → now+16ms (piso); pausado → None
#[test] fn fade_alpha_curve()               // 0ms→0.0; 180ms→1.0; 90ms→0.875; monotônica
```
- [ ] **Passo 2:** `H 'cargo test -- config timing'` → FAIL. **Passo 3:** implementar (parser no molde de `~/Projetos/logi-osd/src/config.rs`). **Passo 4:** PASS.
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `src/config.rs`, `src/timing.rs`, `data/config.exemplo`, `src/main.rs`.

---

### Tarefa 4: [opus] Store do protocolo — estados e sinais

Núcleo da semântica (anti-objetivo "perder notificações"). Sem I/O, sem tempo: quem chama decide quando expira.

**Files:**
- Create: `src/store.rs`

**Interfaces:**
- Consumes: `Notification`, `Hints`, `Action` (T2).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum CloseReason { Expired = 1, Dismissed = 2, Closed = 3 }
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum Effect {
      Show(u32), Update(u32), Hide(u32),
      Closed(u32, CloseReason), ActivationToken(u32, String), ActionInvoked(u32, String),
  }
  #[derive(Debug, Clone, PartialEq)] pub struct NewNotification {
      pub app_name: String, pub app_icon: String, pub summary: String, pub body: String,
      pub actions: Vec<Action>, pub hints: Hints, pub expire_timeout: i32,
  }
  pub const HIDDEN_MAX: usize = 200;
  pub struct Store { /* next_id, stack: Vec<Notification> (mais antiga primeiro), pill: Option<Notification>, hidden: VecDeque<Notification>, max_visible */ }
  impl Store {
      pub fn new(max_visible: usize) -> Store;
      #[cfg(test)] pub fn with_next_id(max_visible: usize, next: u32) -> Store;
      pub fn set_max_visible(&mut self, n: usize) -> Vec<Effect>;
      pub fn notify(&mut self, n: NewNotification, replaces_id: u32, dnd: bool) -> (u32, Vec<Effect>);
      pub fn expire(&mut self, id: u32) -> Vec<Effect>;
      pub fn dismiss(&mut self, id: u32) -> Vec<Effect>;
      pub fn close(&mut self, id: u32) -> Vec<Effect>;
      pub fn invoke(&mut self, id: u32, key: &str, token: Option<String>) -> Vec<Effect>;
      pub fn hide_all(&mut self) -> Vec<Effect>;
      pub fn get(&self, id: u32) -> Option<&Notification>;   // visível, pílula ou oculta
      pub fn stack(&self) -> Vec<u32>;                       // visíveis, mais antiga primeiro
      pub fn pill(&self) -> Option<u32>;
      pub fn visible_with_sync_key(&self, key: &str) -> Option<u32>;
  }
  ```
- Regras (as que os testes não determinam sozinhos):
  - ids a partir de 1, crescentes, `u32::MAX` volta para 1 (nunca 0). `replaces_id ≠ 0` usa esse id: se visível/pílula → `Update`; se oculto ou desconhecido → mostra de novo com esse id (`Show`), como o upstream.
  - `sync_key` igual a de uma visível (ou da pílula) → substitui em lugar, mesmo id, `Update`.
  - Pílula (`is_pill`) ocupa um lugar próprio, fora do limite da pilha; pílula nova com outro id → `Hide(antiga)`, `Closed(antiga, Expired)`, `Show(nova)`.
  - Pilha cheia → a mais antiga sai: `Hide(id)` e vai para a lista oculta (transient: `Closed(id, Expired)` e some).
  - Ao entrar na lista oculta, `hints.image = None` (não guardar pixels).
  - `dnd = true`: retorna id, nenhum `Show`; não-transient vai para a oculta; transient → `Closed(id, Expired)` imediato.
  - `invoke`: chave presente nas ações, ou `"default"` → `[ActivationToken?, ActionInvoked, Hide se visível, Closed(Dismissed)]` e remove; `"default"` sem ação default → igual a `dismiss`; chave inexistente → `[]`.
  - `dismiss`/`close` de id desconhecido → `[]`.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn ids_start_at_one_and_increase()
#[test] fn ids_wrap_skipping_zero()                       // with_next_id(4, u32::MAX): ids MAX, 1
#[test] fn replaces_visible_keeps_id_and_updates()        // [Update(1)]; get(1).summary novo
#[test] fn replaces_hidden_shows_again_same_id()
#[test] fn replaces_unknown_id_creates_with_that_id()     // replaces 77 → id 77, Show(77)
#[test] fn sync_key_replaces_visible_in_place()
#[test] fn fifth_card_pushes_oldest_to_hidden_without_signal() // efeitos == [Hide(1), Show(5)]; get(1) existe; nenhum Closed
#[test] fn burst_keeps_four_visible_and_all_ids_unique()  // 50 notify → stack().len()==4, ids 1..=50 distintos, get(1) Some
#[test] fn expire_hides_without_signal_and_stays_invokable() // notificação com ação default; expire(1)==[Hide(1)]; invoke(1,"default",None)==[ActionInvoked(1,"default"),Closed(1,Dismissed)]
#[test] fn transient_expire_emits_reason_1_and_forgets()  // [Hide(1), Closed(1,Expired)]; get(1) None
#[test] fn dismiss_visible_and_hidden_reason_2()
#[test] fn close_reason_3_visible_and_hidden_unknown_silent()
#[test] fn invoke_with_token_emits_token_first()          // [ActivationToken(1,"t"), ActionInvoked(1,"reply"), Hide(1), Closed(1,Dismissed)]
#[test] fn invoke_unknown_action_is_ignored()             // invoke(1,"nao-existe",None)==[] e 1 continua visível
#[test] fn invoke_unknown_id_is_silent()
#[test] fn default_without_default_action_dismisses()
#[test] fn hidden_list_caps_at_200_and_drops_images()     // 205 expirados → o 1º sumiu; hints.image None nos ocultos
#[test] fn dnd_accepts_without_showing()                  // (id, []) e invoke funciona depois
#[test] fn dnd_transient_closes_immediately()             // [Closed(id, Expired)]
#[test] fn hide_all_moves_visible_to_hidden()             // [Hide(1),Hide(2)], sem Closed; pílula transient → Closed(Expired)
#[test] fn pill_slot_is_separate_and_replaced()           // 4 cartões + pílula → 4 na pilha; pílula nova sem chave → Hide/Closed(Expired) da antiga + Show
#[test] fn pill_same_sync_key_updates_same_id()
#[test] fn set_max_visible_shrinks_stack()                // 4 → 2: Hide das 2 mais antigas
```
- [ ] **Passo 2:** `H 'cargo test store'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `src/store.rs`, `src/main.rs`.

---

### Tarefa 5: [opus] Servidor D-Bus no calloop — libdbus, nome, barramento privado

Primeira versão executável: `notificacoes --sem-tela` (D-Bus + Store + timers de expiração, sem Wayland). É o que os testes de integração dos dois planos usam.

**Files:**
- Create: `src/dbus/mod.rs`, `src/dbus/server.rs` (decodificação/codificação, pura), `src/dbus/conn.rs` (fd no calloop), `src/panel.rs`, `src/headless.rs`
- Modify: `src/main.rs` (args `--sem-tela`; `State`; funil `apply_effects`), `Cargo.toml` (+`dbus`, +`smithay-client-toolkit` só para o `calloop` reexportado)
- Test: `tests/common/mod.rs`, `tests/dbus.rs`

**Interfaces:**
- Consumes: `Store`, `Effect`, `NewNotification`, `CloseReason` (T4); `Hints::parse`, `parse_actions`, `HintValue`, `RawImage` (T2); `plain` (T2); `display_duration`, `Config` (T3).
- Produces:
  ```rust
  // dbus/server.rs
  pub const NAME: &str = "org.freedesktop.Notifications";
  pub const PATH_FDO: &str = "/org/freedesktop/Notifications";     pub const IFACE_FDO: &str = "org.freedesktop.Notifications";
  pub const PATH_APPLET: &str = "/com/system76/NotificationsApplet"; pub const IFACE_APPLET: &str = "com.system76.NotificationsApplet";
  pub const CAPABILITIES: &[&str] = &["actions","action-icons","body","body-markup","icon-static","persistence","sound"];
  #[derive(Debug, Clone, PartialEq)] pub enum Call {
      Notify { app_name: String, replaces_id: u32, app_icon: String, summary: String, body: String,
               actions: Vec<String>, hints: Vec<(String, HintValue)>, expire_timeout: i32 },
      CloseNotification(u32), GetCapabilities, GetServerInformation, InvokeAction(u32, String),
      Introspect, Ping, GetMachineId,
  }
  #[derive(Debug)] pub struct DecodeError { pub name: &'static str, pub text: String } // org.freedesktop.DBus.Error.{UnknownMethod,InvalidArgs,UnknownObject}
  pub fn decode(msg: &dbus::Message) -> Result<Call, DecodeError>;
  pub fn introspect_xml(path: &str) -> &'static str;
  pub fn effect_signal(e: &Effect) -> Option<dbus::Message>;   // Closed/ActionInvoked/ActivationToken → sinal em PATH_FDO
  #[derive(Debug, Clone, PartialEq)] pub struct NotifyArgs { pub app_name: String, pub replaces_id: u32, pub app_icon: String, pub summary: String, pub body: String, pub hints: Vec<(String, HintValue)>, pub expire_timeout: i32 }
  pub fn notify_call(args: &NotifyArgs) -> dbus::Message;      // chamada Notify para NAME (USB e testes)
  // dbus/conn.rs
  pub struct Bus { /* LocalConnection */ }
  impl Bus {
      /// Conecta à sessão, pede o nome (flags das Global Constraints), registra o fd do watch no calloop.
      pub fn session(handle: &LoopHandle<'static, State>) -> Result<Bus, dbus::Error>;
      pub fn reply(&self, to: &dbus::Message, reply: dbus::Message);
      pub fn emit(&self, e: &Effect);
      pub fn send(&self, msg: dbus::Message);
  }
  // panel.rs
  pub fn hold_panel_fd() -> Option<std::os::fd::OwnedFd>;
  // main.rs
  pub struct State { /* store, cfg, bus: Option<Bus>, screen: Screen, dnd: bool, handle, _panel_fd */ }
  pub enum Screen { Headless(headless::Headless) /* T9 acrescenta Wayland(…) */ }
  impl State { pub fn on_call(&mut self, msg: dbus::Message, call: Call); pub fn apply_effects(&mut self, effects: Vec<Effect>); pub fn on_expire(&mut self, id: u32); }
  ```
- Integração com o calloop (decisão, não determinada pelos testes): `Channel::get_private(BusType::Session)`, `set_watch_enabled(true)`, fd de `watch()` num `Generic` `Interest::READ`, `Mode::Level`. No callback: `read_write(Some(Duration::ZERO))` e drenar `pop_message()` até `None`; **drenar também logo após registrar e após qualquer chamada bloqueante** (libdbus pode ter lido mensagens para o buffer interno sem o fd ficar legível). Depois de enviar: `channel().flush()`. Ignorar `method_return`/`error`/sinais recebidos (inclui a resposta das próprias chamadas `Notify` da T14). `RequestName` com resposta `InQueue` → loga e segue (o sinal `NameAcquired` chega depois). Sem barramento de sessão → loga e segue sem `Bus` (não sai).
- `apply_effects`: `Closed`/`ActionInvoked`/`ActivationToken` → `bus.emit`, na ordem recebida; `Show`/`Update`/`Hide` → `screen`. `Headless` mantém `Countdown` por id visível e **um** `Timer` calloop para o menor prazo; sem visível, sem timer.
- `Notify`: corpo passa por `plain`; responde o id antes de aplicar os efeitos. Com `NOTIFICACOES_DEBUG=1` no ambiente, imprime `notificacoes: notify id=<n> app=<app_name>` no stderr (usado pelos testes de integração dos dois planos; T13 e T16 acrescentam linhas próprias).
- O crate é só binário: testes que precisam de símbolos internos ficam em `#[cfg(test)]` dentro do módulo; `tests/*.rs` só falam com o processo (`CARGO_BIN_EXE_notificacoes`) pelo barramento.
- `tests/common/mod.rs`: `pub struct PrivateBus { pub address: String, child: Child }` — `dbus-daemon --session --nofork --print-address=1`, lê a primeira linha; `Drop` mata. `pub fn spawn_daemon(bus: &PrivateBus, xdg_config_home: &Path) -> Child` usa `env!("CARGO_BIN_EXE_notificacoes")` com `--sem-tela`, `DBUS_SESSION_BUS_ADDRESS` e `XDG_CONFIG_HOME` de um diretório em `env!("CARGO_TARGET_TMPDIR")`; espera o nome ter dono (≤ 2 s). Cliente nos testes: `dbus::blocking::Connection` via `Channel::open_private(&address)` + `register()`.

- [ ] **Passo 1: Testes que falham** (unitários em `server.rs`; integração em `tests/dbus.rs`, cada teste com o próprio `PrivateBus`)

```rust
// server.rs
#[test] fn decode_notify_full_signature()              // Message montada com (s,u,s,s,s,as,a{sv},i) → Call::Notify com hints na ordem
#[test] fn decode_rejects_wrong_signature()            // Notify com (s) → DecodeError{name: "org.freedesktop.DBus.Error.InvalidArgs"}
#[test] fn image_data_struct_to_raw_image()            // a{sv} com image-data (iiibiiay) → HintValue::Image(RawImage{..})
#[test] fn image_data_wrong_signature_ignored()        // (iii) → dica ausente
#[test] fn byte_hint_becomes_int()                     // urgency como y(2) → Int(2)
#[test] fn unknown_method_error()                      // member "Foo" → UnknownMethod
#[test] fn invoke_action_on_applet_path()              // PATH_APPLET/IFACE_APPLET/InvokeAction(u,s) → Call::InvokeAction
#[test] fn introspect_lists_methods_and_signals()      // XML de PATH_FDO contém Notify, ActivationToken; de PATH_APPLET contém InvokeAction
// tests/dbus.rs
#[test] fn notify_returns_increasing_ids()
#[test] fn capabilities_and_server_information()      // lista exata e ("notificacoes","Aenes",<versão>,"1.2")
#[test] fn close_notification_emits_reason_3()
#[test] fn invoke_action_emits_action_then_closed_2()  // via InvokeAction no PATH_APPLET; sinais em ordem
#[test] fn expired_card_emits_nothing_and_stays_invokable() // expire_timeout=300; nenhum sinal em 600 ms; InvokeAction depois → ActionInvoked + Closed(2)
#[test] fn transient_expires_with_reason_1()           // hint transient=true, expire 300 → NotificationClosed(id,1)
#[test] fn no_reply_expected_gets_no_reply()           // Notify com set_no_reply(true) → nenhuma method_return em 300 ms; o seguinte com resposta recebe id 2
#[test] fn takes_name_from_owner_that_allows_replacement() // dono prévio com allow_replacement=true → daemon vira dono
#[test] fn waits_in_queue_when_owner_forbids_replacement() // dono prévio sem allow → daemon na fila (vivo); dono solta → daemon vira dono; Notify responde
#[test] fn panel_fd_is_kept_and_cloexec()              // spawn com DAEMON_NOTIFICATIONS_FD herdado (socketpair) → /proc/PID/fd contém o socket com FD_CLOEXEC (lido em /proc/PID/fdinfo flags)
```
- [ ] **Passo 2:** `H 'cargo test'` → FAIL. **Passo 3:** implementar. **Passo 4:** `H 'cargo test'` → PASS.
- [ ] **Passo 5 (ao vivo, sem tela):** `H 'cargo build --release && (target/release/notificacoes --sem-tela & sleep 1; busctl --user status org.freedesktop.Notifications | grep -E "^PID|^Comm"; notify-send -t 2000 teste; dbus-monitor --session "interface=org.freedesktop.Notifications" & sleep 3; pkill -x notificacoes)'` → `Comm=notificacoes`; depois `H 'pkill -x cosmic-notifications'` para devolver o nome ao oficial (o painel pisca). Esperado: `notify-send` retorna, nenhum `NotificationClosed` ao expirar.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/dbus/*`, `src/panel.rs`, `src/headless.rs`, `src/main.rs`, `Cargo.toml`, `tests/common/mod.rs`, `tests/dbus.rs`.

---

### Tarefa 6: [sonnet] Texto — fallback, emoji, reticências

**Files:**
- Create: `src/render/text.rs`
- Modify: `Cargo.toml` (+`cosmic-text`)

**Interfaces:**
- Consumes: `Rgba`, `top_level_field`, `latest_version_dir` (T2); `resultados-spike.txt: cosmic_text_*` (T1).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct TextStyle { pub px: f32, pub weight: u16, pub line_height: f32 }
  pub const TITLE: TextStyle  = TextStyle { px: 13.0, weight: 600, line_height: 1.3 };
  pub const APP: TextStyle    = TextStyle { px: 11.0, weight: 400, line_height: 1.3 };
  pub const BODY: TextStyle   = TextStyle { px: 12.0, weight: 400, line_height: 1.35 };
  pub const BUTTON: TextStyle = TextStyle { px: 13.0, weight: 600, line_height: 1.2 };
  pub struct Text { /* FontSystem, família, Option<SwashCache> */ }
  pub struct Laid { pub width: f32, pub height: f32, pub lines: usize, /* Buffer em px de dispositivo */ }
  impl Text {
      pub fn new(family: Option<&str>) -> Text;
      /// max_width e resultado em px lógicos; layout feito em px de dispositivo (px * scale).
      pub fn layout(&mut self, s: &str, style: TextStyle, max_width: f32, max_lines: usize, scale: u32) -> Laid;
      pub fn draw(&mut self, pm: &mut Pixmap, laid: &Laid, x: f32, y: f32, scale: u32, color: Rgba); // x,y lógicos
      pub fn release_caches(&mut self);  // solta SwashCache; chamado quando a tela esvazia
  }
  /// Família de `~/.config/cosmic/com.system76.CosmicTk/v*/interface_font` (campo `family`), se houver.
  pub fn interface_font_family(cosmic_dir: &Path) -> Option<String>;
  ```
- Decisão: `Wrap::WordOrGlyph`, `Ellipsize::End(EllipsizeHeightLimit::Lines(max_lines))`; família = `cfg.font` → `interface_font_family` → `"Open Sans"`. Emoji colorido: na rasterização, `Content::Color` copia RGBA; máscara pinta com `color`. **Modo reduzido** (só se o portão da T1 mandar): `FontSystem::new_with_locale_and_db` com uma `fontdb::Database` carregada só destes arquivos, os que existirem: `/usr/share/fonts/TTF/OpenSans-Regular.ttf`, `/usr/share/fonts/TTF/OpenSans-SemiBold.ttf`, `/usr/share/fonts/noto/NotoSans-Regular.ttf`, `/usr/share/fonts/noto/NotoColorEmoji.ttf`, `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, `/usr/share/fonts/noto/NotoSans{Arabic,Hebrew,Devanagari,Thai}-Regular.ttf`.
- Os testes dependem das fontes do sistema (`ttf-opensans`, `noto-fonts`, `noto-fonts-emoji`, `noto-fonts-cjk`); rodam no host.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn title_one_line_ellipsized_within_width() // texto de 200 chars, max_width 280, 1 linha → lines==1, width <= 280
#[test] fn body_three_lines_then_ellipsis()        // 600 chars → lines==3; desenhado: a última linha termina com tinta à direita (reticências) e há 3 faixas de tinta
#[test] fn short_text_is_one_line_and_narrow()     // "Oi" → lines 1, width < 40
#[test] fn empty_text_has_zero_height()
#[test] fn emoji_renders_color_pixels()            // "🙂" a 24 px → existe pixel com max(r,g,b)-min(r,g,b) > 60
#[test] fn cjk_and_arabic_render_ink()             // "你好" e "مرحبا" → cada um > 30 pixels com alfa > 128
#[test] fn scale_two_doubles_ink_width()           // largura da tinta em escala 2 ≈ 2× a da escala 1 (±2 px)
#[test] fn interface_font_family_from_ron()        // fixture tests/fixtures/theme/cosmictk_interface_font com (family: "Inter", …) → Some("Inter"); ausente → None
```
- [ ] **Passo 2:** `H 'cargo test text'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `src/render/text.rs`, `src/render/mod.rs`, `Cargo.toml`, fixture nova.

---

### Tarefa 7: [sonnet] Ícones de app — tema, .desktop, SVG

**Files:**
- Create: `src/icons.rs`, `tests/fixtures/icons/` (tema `Teste` com `index.theme` herdando `Pai`; `Teste/48x48/apps/app-a.png` 48×48 gerado com `magick`; `Pai/scalable/apps/app-b.svg` escrito à mão; `Pai/scalable/status/algo-symbolic.svg` cinza `#bebebe`; `hicolor/index.theme`; `hicolor/32x32/apps/app-c.png`), `tests/fixtures/applications/` (`org.exemplo.Chat.desktop` com `Icon=app-a`, `Name=Chat`, `StartupWMClass=chatapp`; `brave-abc.desktop` com ação `[Desktop Action x]` contendo outro `Icon=`)
- Modify: `Cargo.toml` (+`resvg`, +`png`)

**Interfaces:**
- Consumes: `Notification`, `ImageHint` (T2); `Rgba`, `latest_version_dir`, `top_level_field` (T2).
- Produces:
  ```rust
  #[derive(Debug, Clone)] pub struct IconRoots { pub app_dirs: Vec<PathBuf>, pub icon_dirs: Vec<PathBuf> }
  impl IconRoots { pub fn from_env() -> IconRoots; } // app_dirs: $XDG_DATA_HOME, $XDG_DATA_DIRS, flatpak exports (como a Central) + "/applications"; icon_dirs: ~/.icons, <data dirs>/icons, /usr/share/pixmaps
  pub fn desktop_icon(entries: &[DesktopEntry], app: &str, desktop_entry: Option<&str>) -> Option<String>; // porte de quick-settings/src/notifications.rs:desktop_icon
  pub struct DesktopEntry { pub id: String, pub name: String, pub wm_class: String, pub icon: String }
  pub fn load_entries(roots: &IconRoots) -> Vec<DesktopEntry>;
  pub fn find_icon(roots: &IconRoots, theme: &str, name: &str, size: u32, scale: u32) -> Option<PathBuf>;
  pub fn rasterize(path: &Path, px: u32, tint: Option<Rgba>) -> Option<Pixmap>; // PNG ou SVG, contido em px×px
  pub fn icon_theme_name(cosmic_dir: &Path) -> String;  // CosmicTk/v*/icon_theme (string RON entre aspas), padrão "Cosmic"
  pub struct Icons { /* roots, theme, entries carregadas na 1ª vez, LRU de 32 (nome|caminho, px) → Option<Pixmap> */ }
  impl Icons {
      pub fn new(roots: IconRoots, theme: String) -> Icons;
      pub fn app_icon(&mut self, n: &Notification, px: u32, tint: Rgba) -> Option<Pixmap>;
      pub fn named(&mut self, name: &str, px: u32, tint: Rgba) -> Option<Pixmap>;
      pub fn clear(&mut self);  // solta o LRU (tela vazia)
  }
  ```
- `find_icon` segue a spec de temas: cadeia `tema → Inherits (recursivo, sem repetir) → "Cosmic" → "hicolor"`; em cada tema lê `index.theme` (`Directories` + `ScaledDirectories`, `Size`, `Scale`, `Type` Fixed/Scalable/Threshold, `MinSize`, `MaxSize`, `Threshold`), escolhe a correspondência exata e senão a de menor distância; extensões `png` e `svg`; por último `/usr/share/pixmaps/<nome>.(png|svg)`. Nome que é caminho absoluto ou `file://` → o próprio arquivo, se existir.
- `app_icon`: ordem `desktop-entry` → nome do app → `app_icon` (nome, caminho ou URI) → `ImageHint::IconName`. Nome terminado em `-symbolic` é tingido com `tint` (preserva o alfa, troca a cor).

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn desktop_entry_hint_wins()                 // desktop_entry Some("org.exemplo.Chat") → "app-a"
#[test] fn app_name_matches_name_or_wm_class()       // "Chat" e "chatapp" → "app-a"
#[test] fn flatpak_style_suffix_matches()            // desktop_entry "Chat" casa com id "org.exemplo.chat" pelo último segmento
#[test] fn desktop_action_icon_is_ignored()          // brave-abc: Icon do [Desktop Entry], não o da ação
#[test] fn exact_size_png_found()                    // find_icon(Teste, app-a, 48, 1) → .../48x48/apps/app-a.png
#[test] fn inherits_then_hicolor()                   // app-b vem de Pai/scalable; app-c de hicolor
#[test] fn absolute_path_and_file_uri()              // caminho existente → ele; inexistente → None
#[test] fn svg_rasterized_to_requested_size()        // rasterize(app-b.svg, 40) → 40×40 com tinta
#[test] fn symbolic_is_tinted()                      // algo-symbolic com tint (255,0,0) → pixels opacos vermelhos
#[test] fn missing_icon_is_none()
#[test] fn icon_theme_from_cosmic_config()           // fixture "\"Breeze-Fino\"" → "Breeze-Fino"; ausente → "Cosmic"
```
- [ ] **Passo 2:** `H 'cargo test icons'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `src/icons.rs`, `Cargo.toml`, `tests/fixtures/icons/`, `tests/fixtures/applications/`.

---

### Tarefa 8: [sonnet] Desenho do cartão — geometria e pixels

**Files:**
- Create: `src/render/shapes.rs` (retângulo arredondado, sombra gaussiana, pílula, ×), `src/render/card.rs`

**Interfaces:**
- Consumes: `Text`, `TextStyle`, consts (T6); `Theme`, `Rgba`, `WARNING`, `CRITICAL` (T2); `Level`, `Action` (T2); `Icons` (T7).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq)] pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }
  impl Rect { pub fn contains(&self, x: f32, y: f32) -> bool; }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Format { Card, Compact, Image }
  #[derive(Debug, Clone, PartialEq, Eq)] pub enum Hit { Close, Action(String), Default }
  pub enum ActionButton { Text { key: String, label: String, primary: bool }, Icon { key: String, label: String, icon: Pixmap } }
  pub struct CardView<'a> {
      pub format: Format, pub app: &'a str, pub title: &'a str, pub body: &'a str, pub buttons: &'a [ActionButton],
      pub icon: Option<&'a Pixmap>, pub thumb: Option<&'a Pixmap>, pub dim_icon: bool, pub level: Level, pub redacted: bool,
  }
  pub struct CardLayout { pub card: Rect, pub surface: (u32, u32), pub regions: Vec<(Rect, Hit)>, pub bar: Rect /* + textos dispostos */ }
  pub const CARD_W: f32 = 340.0; pub const PAD: f32 = 10.0; pub const RADIUS: f32 = 16.0; pub const ICON: f32 = 40.0;
  pub const ROUND_BTN: f32 = 30.0; pub const BTN_H: f32 = 26.0; pub const BAR_H: f32 = 3.0; pub const GAP: f32 = 8.0;
  pub struct ShadowPad { pub l: f32, pub t: f32, pub r: f32, pub b: f32 }
  pub const SHADOW_PAD: ShadowPad = ShadowPad { l: 20.0, t: 12.0, r: 20.0, b: 24.0 };
  pub const AGORA: &str = "agora"; pub const NOVA_NOTIFICACAO: &str = "Nova notificação";
  pub struct Palette { pub card_bg: Rgba, pub border: Rgba, pub text: Rgba, pub text_dim: Rgba, pub body: Rgba, pub button: Rgba, pub button_hover: Rgba, pub level: Rgba, pub on_level: Rgba }
  pub fn palette(theme: &Theme, level: Level) -> Palette;
  pub fn buttons(actions: &[Action], action_icons: bool, icons: &mut Icons, tint: Rgba) -> Vec<ActionButton>; // sem "default", máx 3, 1º texto = primary
  pub fn layout(text: &mut Text, v: &CardView, scale: u32) -> CardLayout;
  pub fn render(text: &mut Text, v: &CardView, l: &CardLayout, theme: &Theme, hover: Option<&Hit>, scale: u32) -> Pixmap; // sem a barra
  pub fn draw_bar(pm: &mut Pixmap, l: &CardLayout, theme: &Theme, level: Level, fraction: f32, scale: u32);
  ```
- Disposição (lógica, origem no canto do cartão; `card` fica deslocado por `SHADOW_PAD` dentro da superfície):
  - `Card`: ícone 40 em (10,10); à direita (x=60): linha `App · agora` (APP, cor `text_dim`), título (TITLE, 1 linha), corpo (BODY, até 3 linhas); × redondo 30 no canto superior direito (x = 340−10−30, y=10), região `Hit::Close`; resto do cartão = `Hit::Default`. Botões numa linha abaixo do texto, alinhados à direita, altura 26, gap 8, rótulo até 140 px com "…"; não cabem → saem do fim, o principal fica. Barra embaixo (8 de espaço, 3 de altura, largura 340−20).
  - `Compact`: ícone 32 (apagado = alfa 45% quando `dim_icon`), título (TITLE) e subtítulo = corpo (APP, 1 linha); sem × e sem botões; cartão inteiro = `Hit::Default` (clique dispensa). Barra embaixo.
  - `Image` (completada na T11): aqui só reserva o caso; `layout` com `Format::Image` e `thumb = None` cai em `Card`.
  - `redacted`: título = nome do app, corpo = `NOVA_NOTIFICACAO`, sem miniatura e sem botões; × mantido.
- Sombra: máscara do retângulo arredondado borrada (3 passadas de box blur equivalentes a σ = 14·escala), deslocada 8·escala para baixo, preta a 45%, desenhada antes do cartão.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn card_width_340_and_surface_adds_pad()     // l.card.w == 340; l.surface.0 == 380 (escala 1)
#[test] fn card_height_grows_with_body_lines()       // corpo 1 linha < corpo 3 linhas
#[test] fn outside_rounded_corner_has_only_shadow()  // pixel em (card.x+1, card.y+1): alfa < 0,5 do fundo
#[test] fn center_has_dark_background()              // tema escuro: pixel no meio de área sem texto ≈ #1e1f24 (±3) com alfa ≈ 240
#[test] fn primary_button_uses_level_color()         // Normal → accent do tema; Atencao → WARNING; Critico → CRITICAL (pixel no meio do botão)
#[test] fn bar_fraction_proportional()               // fraction 0,5 → largura acesa ≈ metade da barra (±2 px)
#[test] fn regions_close_buttons_default()           // ações [default, reply] → regiões Close, Action("reply"), Default; nenhuma Action("default")
#[test] fn at_most_three_buttons()                   // 5 ações → 3 regiões Action
#[test] fn redacted_has_no_buttons_and_generic_text() // nenhum Action; layout com título = app
#[test] fn compact_has_no_close_and_dims_icon()      // nenhuma região Close; pixel do ícone com alfa ≤ 45% do original
#[test] fn scale_two_doubles_surface()               // escala 2 → surface == (760, 2×altura)
#[test] fn hover_lightens_button()                   // render com hover Some(Action("reply")) → pixel do botão mais claro que sem hover
#[test] fn light_theme_uses_theme_background()       // fixture light → fundo ≈ background.base do tema claro
#[test] #[ignore] fn dump_pngs()                     // grava docs/verificacao/cartoes/{normal,atencao,critico,compacto,privado}.png
```
- [ ] **Passo 2:** `H 'cargo test card'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS. **Passo 5:** `H 'cargo test dump_pngs -- --ignored'` e conferir os PNGs ao lado de um print do `print-preview` (`H '~/.local/bin/print-preview ~/Imagens/Screenshots/<algum>.png & sleep 1; grim -g "$(slurp)" docs/verificacao/cartoes/referencia-print-preview.png'`, com o usuário).
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/render/{shapes,card,mod}.rs`, `docs/verificacao/cartoes/`.

---

### Tarefa 9: [opus] Superfícies Wayland e pilha — várias superfícies, timer único

**Files:**
- Create: `src/wayland/mod.rs` (conexão, globais, handlers sctk), `src/wayland/surface.rs` (porte de `~/Projetos/logi-osd/src/osd/wayland.rs`, generalizado para N superfícies), `src/presenter.rs`, `tools/dev.sh`
- Modify: `src/main.rs` (`Screen::Wayland`, modo padrão e `--demo`), `Cargo.toml`

**Interfaces:**
- Consumes: `Store`, `Effect` (T4); `State`, `apply_effects`, `on_expire` (T5); `Countdown`, `display_duration`, `next_bar_step`, `fade_alpha`, `FADE_IN` (T3); `layout`, `render`, `draw_bar`, `CardView`, `CardLayout`, `SHADOW_PAD`, `buttons` (T8); `Text` (T6); `Icons` (T7); `theme::load` (T2).
- Produces:
  ```rust
  // presenter.rs (puro)
  /// Margem inferior de cada superfície, mais nova primeiro (índice 0 = embaixo).
  /// margem_i = base − pad_b + Σ_{j<i}(card_heights[j] + gap)  (alturas do cartão, sem o enchimento de sombra)
  pub fn stack_margins(card_heights: &[u32], base: i32, gap: i32, pad_b: i32) -> Vec<i32>;
  pub struct CardTiming { pub countdown: Countdown, pub bar_device_px: u32, pub shown_at: Instant }
  /// Próximo despertar: menor entre prazos, passos de barra e fim de fade; None sem cartão/pílula.
  pub fn next_wake(cards: &[CardTiming], now: Instant) -> Option<Instant>;
  // wayland/mod.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub enum SurfaceKey { Card(u32), Pill }
  pub struct Ui { /* globais, superfícies por chave, saída da pilha, escala por superfície */ }
  impl Ui {
      pub fn connect(handle: &LoopHandle<'static, State>) -> Result<Ui, Box<dyn std::error::Error>>;
      pub fn open(&mut self, key: SurfaceKey, anchor: Anchor, size: (u32, u32), margin: (i32, i32, i32, i32), input: Option<Rect>, stack_output: bool);
      pub fn present(&mut self, key: SurfaceKey, pm: &Pixmap, scale: u32, damage: Option<Rect>);
      pub fn set_margin(&mut self, key: SurfaceKey, margin: (i32, i32, i32, i32));
      pub fn close(&mut self, key: SurfaceKey);
      pub fn scale(&self, key: SurfaceKey) -> u32;
      pub fn is_empty(&self) -> bool;
  }
  // main.rs
  pub struct CardState { pub id: u32, pub layout: CardLayout, pub static_pm: Pixmap, pub timing: CardTiming, pub hover: Option<Hit>, pub scale: u32 }
  ```
- Regras de integração (o que os testes não fixam):
  - `Show(id)`: monta `CardView` (formato `Card` ou `Compact` por `estilo`), `layout`, `render` (estático, sem barra) → guarda em `CardState`; `open` com `Anchor::BOTTOM | Anchor::RIGHT`, tamanho = `surface`, margem direita `24 − SHADOW_PAD.r`, input = `card`, `stack_output = true`; recalcula `stack_margins` de todos e chama `set_margin`. `Update(id)`: refaz o estático e reinicia o `Countdown` (`restart`). `Hide(id)`: `close` e recalcula margens.
  - Saída: o primeiro cartão abre sem saída; quando o compositor manda `surface_enter`, a `WlOutput` vira a "saída da pilha" e os cartões seguintes abrem nela (`stack_output = true`) enquanto houver cartão; pilha vazia esquece a saída.
  - Quadro: cópia do estático + `draw_bar` + (durante o fade) alfa global `fade_alpha`; `present` com `damage = Some(bar)` fora do fade.
  - **Um** `Timer` calloop armado para `next_wake`; ao disparar: expira quem venceu (`State::on_expire` → `Store::expire`), redesenha barras que mudaram, rearma; `None` → solta o timer. Tela vazia (`ui.is_empty()`): `text.release_caches()`, `icons.clear()`, `libc::malloc_trim(0)`.
  - Tema relido a cada `Show`/`Update` (`theme::load(~/.config/cosmic)`), como no logi-osd.
  - Erro fatal de Wayland (conexão caiu) → `ExitCode::FAILURE`; o resto loga.
  - `--demo`: sem D-Bus; injeta no `Store`, a cada 1,5 s: cartão normal com 2 ações, cartão com corpo de 3 linhas e emoji, atenção, crítico, compacto (USB), e sai 16 s depois do último.
- `tools/dev.sh` (sh, `set -e`): `iniciar` = `cargo build --release`, inicia `target/release/notificacoes` com `setsid`, log em `~/.cache/notificacoes-dev.log`, PID em `~/.cache/notificacoes-dev.pid`, mostra `busctl --user status org.freedesktop.Notifications | grep -E "^PID|^Comm"`; `parar` = mata o PID salvo, depois `pkill -x cosmic-notifications` (a sessão religa o oficial, que pega o nome; o painel pisca); `status` = dono do nome e PIDs. Se `resultados-spike.txt: nome_tomado_do_oficial = nao`, `iniciar` também faz `pkill -x cosmic-notifications` depois de iniciar.

- [ ] **Passo 1: Testes que falham** (`presenter.rs`)

```rust
#[test] fn stack_margins_newest_at_bottom()       // alturas [100, 80], base 24, gap 8, pad_b 24 → [0, 108]
#[test] fn removing_card_recomputes_margins()     // [100,80,90] → [0,108,196]; sem o do meio → [0,108]
#[test] fn next_wake_is_min_of_deadline_and_bar_step()
#[test] fn paused_card_contributes_no_wake()      // único cartão pausado → None
#[test] fn fade_end_wakes_even_with_long_bar_step()
#[test] fn no_cards_no_wake()
```
- [ ] **Passo 2:** `H 'cargo test presenter'` → FAIL. **Passo 3:** implementar tudo. **Passo 4:** `H 'cargo test'` → PASS.
- [ ] **Passo 5 (ao vivo — demo):** `H 'cargo build --release && (target/release/notificacoes --demo & sleep 8; grim docs/verificacao/t9-pilha.png)'` → o PNG mostra 4 cartões empilhados embaixo à direita, mais novo embaixo, barra correndo, sombra suave. Conferir com o usuário ao lado da referência da T8.
- [ ] **Passo 6 (ao vivo — modo dev + repouso):** `H 'tools/dev.sh iniciar; notify-send -A ok=OK "Teste" "corpo"; sleep 10; pid=$(cat ~/.cache/notificacoes-dev.pid); cs(){ for t in /proc/$pid/task/*/status; do grep -E "^(voluntary|nonvoluntary)_ctxt_switches" $t; done | awk "{s+=\$2} END {print s}"; }; a=$(cs); sleep 30; b=$(cs); echo ctx_30s=$((b-a)); grep -E "VmRSS|Threads" /proc/$pid/status; tools/dev.sh parar'` → `ctx_30s=0`, `Threads: 1`.
- [ ] **Passo 7: Deixar no working tree (não commitar)** — `src/wayland/{mod,surface}.rs`, `src/presenter.rs`, `src/main.rs`, `Cargo.toml`, `tools/dev.sh`, `docs/verificacao/t9-pilha.png`.

---

### Tarefa 10: [opus] Ponteiro e ações — hover, clique, ativação

**Files:**
- Create: `src/wayland/pointer.rs`
- Modify: `src/wayland/mod.rs` (seat, `cursor_shape`, `activation`), `src/presenter.rs`, `src/main.rs` (cliques, `InvokeAction` passa a pedir token)

**Interfaces:**
- Consumes: `CardLayout`, `Hit`, `Rect` (T8); `Countdown::pause/resume` (T3); `Store::{dismiss, invoke}` (T4); `Ui`, `SurfaceKey`, `CardState` (T9).
- Produces:
  ```rust
  // presenter.rs
  pub fn hit(l: &CardLayout, x: f32, y: f32) -> Option<Hit>;    // coordenadas da superfície, lógicas
  #[derive(Debug, Default)] pub struct ClickTracker { /* Hit sob o botão pressionado */ }
  impl ClickTracker { pub fn press(&mut self, h: Option<Hit>); pub fn release(&mut self, h: Option<Hit>) -> Option<Hit>; pub fn cancel(&mut self); }
  // main.rs
  pub struct PendingActivation { pub id: u32, pub key: String, pub deadline: Instant }
  impl State { pub fn on_pointer(&mut self, key: SurfaceKey, ev: PointerEv); pub fn on_token(&mut self, id: u32, token: Option<String>); }
  pub enum PointerEv { Enter(f32, f32, u32 /*serial*/), Motion(f32, f32), Leave, Press(u32 /*serial*/), Release }
  ```
- Regras: `Enter` no cartão → `countdown.pause` (só desse cartão; barra congela); `Leave` → `resume`. Mudança de `Hit` sob o cursor → re-`render` do estático com `hover` e `cursor_shape` `Pointer` sobre região clicável que não seja `Default` em `Card`; `Default` em `Image`/`Compact` também `Pointer`. Clique = `press` e `release` no mesmo `Hit` (botão esquerdo, `BTN_LEFT = 0x110`). `Close` → `Store::dismiss`. `Default`/`Action(k)` → pede token `xdg_activation_v1` (app_id `com.system76.CosmicNotifications`, seat+serial do clique, superfície do cartão) e guarda `PendingActivation` com prazo de **200 ms**; no token (ou no prazo, com `None`) → `Store::invoke(id, key, token)`. `InvokeAction` vindo do D-Bus faz o mesmo sem seat/serial/superfície. Pílula não recebe ponteiro.

- [ ] **Passo 1: Testes que falham** (`presenter.rs`)

```rust
#[test] fn hit_maps_points_to_regions()         // centro do × → Close; centro do botão "reply" → Action("reply"); meio do texto → Default; sombra → None
#[test] fn click_needs_press_and_release_same_hit() // press(Close), release(Close) → Some(Close); press(Close), release(Default) → None
#[test] fn leave_cancels_click()                // press, cancel, release → None
```
- [ ] **Passo 2:** `H 'cargo test presenter'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo, com o usuário):** `H 'tools/dev.sh iniciar; dbus-monitor --session "interface=org.freedesktop.Notifications,type=signal" > docs/verificacao/t10-sinais.log & notify-send -A default=Abrir -A b=Botão "Clique" "teste"'`. O usuário: (1) passa o mouse e para — dois `grim` com 1 s de intervalo (`grim docs/verificacao/t10-hover-{1,2}.png`) mostram a barra do mesmo tamanho; (2) clica "Botão" → log com `ActivationToken` (se o compositor deu token), `ActionInvoked(id,"b")`, `NotificationClosed(id,2)` e o `notify-send` imprime `b`; (3) novo aviso, clica × → só `NotificationClosed(id,2)`; (4) novo aviso, deixa expirar → nenhum sinal; `busctl --user call org.freedesktop.Notifications /com/system76/NotificationsApplet com.system76.NotificationsApplet InvokeAction us <id> b` → `ActionInvoked`. `tools/dev.sh parar`.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/wayland/{pointer,mod}.rs`, `src/presenter.rs`, `src/main.rs`, `docs/verificacao/t10-*`.

---

### Tarefa 11: [sonnet] Imagem grande — decodificação com limites

**Files:**
- Create: `src/images.rs`, `tests/fixtures/images/` (`print.png` 800×450, `foto.jpg` 400×300 e `avatar.png` 64×64 gerados com `H 'magick …'`; `quebrado.png` = 100 bytes aleatórios)
- Modify: `src/render/card.rs` (formato `Image`), `src/main.rs` (escolha de formato no `Show`), `Cargo.toml` (+`zune-jpeg`)

**Interfaces:**
- Consumes: `RawImage`, `ImageHint`, `Notification` (T2); `Format`, `CardView`, `ActionButton`, `layout`/`render` (T8); `Icons::named` (T7).
- Produces:
  ```rust
  pub const THUMB_MAX: (u32, u32) = (320, 180);
  pub const MAX_FILE_BYTES: u64 = 32 << 20; pub const MAX_PIXELS: u64 = 40_000_000; pub const MAX_SIDE: u32 = 16_384;
  pub const SMALL_IMAGE: u32 = 128;
  #[derive(Debug)] pub enum ImageError { TooBig, Unsupported, Invalid(&'static str), Io(std::io::Error) }
  pub fn decode_raw(img: &RawImage) -> Result<Pixmap, ImageError>;   // 8 bits, 3 ou 4 canais, rowstride ≥ w*canais, data ≥ rowstride*(h-1)+w*canais
  pub fn decode_file(path: &Path) -> Result<Pixmap, ImageError>;     // PNG/JPEG pela assinatura; tamanho e dimensões checados ANTES de decodificar
  pub fn fit(src: &Pixmap, max: (u32, u32)) -> Pixmap;               // contém, mantém proporção, nunca amplia
  /// Image se estilo Imagem ou (auto e imagem decodificada com algum lado ≥ SMALL_IMAGE); Card/Compact caso contrário.
  pub fn choose_format(n: &Notification, decoded: Option<&Pixmap>) -> Format;
  ```
- Formato `Image` em `card.rs`: miniatura `fit` em `(320·escala, 180·escala)`, centralizada numa área de largura 320, raio 10, fundo preto 35%, região `Hit::Default`; abaixo, numa linha: título (TITLE) e subtítulo = corpo (APP, 60%) à esquerda; botões à direita na ordem das ações (texto em pílula, principal colorido; `ActionButton::Icon` vira botão redondo 30); × redondo 30 por último — igual ao `print-preview`. Barra embaixo.
- `buttons` (T8) passa a produzir `Icon` quando `hints.action_icons` e `icons.named(key, 16, texto)` acha o ícone; senão `Text`.
- Imagem pequena (< `SMALL_IMAGE` nos dois lados) vira o ícone do cartão comum. Falha de decodificação → cartão comum, log de uma linha. Depois de decodificar arquivo > 1 MB: `libc::malloc_trim(0)`.

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn png_and_jpeg_decode()                      // print.png → 800×450; foto.jpg → 400×300
#[test] fn fit_keeps_aspect_and_never_upscales()      // 800×450 → 320×180; 400×300 → 240×180; 64×64 → 64×64
#[test] fn raw_rgb_and_rgba_decode()                  // RawImage 2×2 RGB e RGBA com rowstride com folga → pixels certos (pré-multiplicados)
#[test] fn raw_image_with_short_buffer_is_rejected()  // data menor que o declarado → Invalid
#[test] fn raw_image_bad_channels_rejected()          // channels 2 ou bits 16 → Unsupported
#[test] fn png_over_pixel_limit_rejected_before_decode() // cabeçalho IHDR 20000×20000 forjado (só 33 bytes) → TooBig sem alocar
#[test] fn garbage_file_is_unsupported()              // quebrado.png → Unsupported/Invalid
#[test] fn missing_file_is_io_error()
#[test] fn small_image_becomes_card_icon()            // avatar 64×64 sem x-estilo → Format::Card
#[test] fn estilo_imagem_forces_image_format()        // avatar com x-estilo=imagem → Format::Image
#[test] fn image_card_layout()                        // largura 340; região Default cobre a miniatura; × é a última região à direita
#[test] fn action_icons_make_round_button()           // action_icons + key "folder-open-symbolic" encontrado → ActionButton::Icon; "editar" → Text primary
```
- [ ] **Passo 2:** `H 'cargo test -- images card'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo):** `H 'tools/dev.sh iniciar; notify-send -a "Captura de tela" -h string:image-path:$PWD/tests/fixtures/images/print.png -h boolean:action-icons:true -A default=Editar -A editar=Editar -A folder-open-symbolic="Abrir pasta" "Captura copiada" "800×450 · salvo em Screenshots" & sleep 1; grim docs/verificacao/t11-imagem.png; tools/dev.sh parar'` → visual igual ao `print-preview` (comparar com `docs/verificacao/cartoes/referencia-print-preview.png`).
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/images.rs`, `src/render/card.rs`, `src/main.rs`, `Cargo.toml`, `tests/fixtures/images/`, `docs/verificacao/t11-imagem.png`.

---

### Tarefa 12: [sonnet] Não perturbe e recarga — inotify simples

**Files:**
- Create: `src/watch.rs` (testes no próprio módulo, em diretórios `std::env::temp_dir()/notificacoes-teste-<pid>-<nome>`)
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Store::hide_all`, `Store::set_max_visible` (T4); `config::load` (T3); `State` (T5).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum WatchTag { Dnd, Config }
  pub fn dnd_dir(cosmic_dir: &Path) -> PathBuf;   // <cosmic>/com.system76.CosmicNotifications/v1
  pub fn read_dnd(dir: &Path) -> bool;            // arquivo "do_not_disturb" com "true" (aparado); resto/ausente → false
  pub struct Watcher { /* inotify fd + wd → tag */ }
  impl Watcher {
      pub fn new(watches: &[(PathBuf, WatchTag)]) -> std::io::Result<Watcher>; // cria os diretórios que faltarem (create_dir_all) antes de vigiar
      pub fn drain(&mut self) -> Vec<WatchTag>;   // tags distintas com evento desde a última chamada (IN_CLOSE_WRITE|IN_MOVED_TO|IN_CREATE|IN_DELETE)
  }
  impl AsFd for Watcher;
  ```
- `main.rs`: `Watcher` no calloop (`Generic`, READ). `Dnd` → relê; virou `true` → `apply_effects(store.hide_all())`. `Config` → `config::load()`, aplica `set_max_visible` e os tempos novos aos próximos cartões. Falha do inotify → loga; DND passa a ser lido a cada `Notify`.

- [ ] **Passo 1: Testes que falham**

```rust
// src/watch.rs
#[test] fn read_dnd_values()                  // "true\n"→true, "false"→false, ausente→false, "lixo"→false
// src/watch.rs, com diretório temporário próprio por teste
#[test] fn change_in_watched_dir_is_reported() // escreve do_not_disturb via arquivo temporário + rename → drain() == [Dnd]
#[test] fn missing_dir_is_created_and_watched()
#[test] fn two_tags_reported_once_each()        // vários eventos nos dois diretórios → [Dnd, Config] sem repetição
```
- [ ] **Passo 2:** `H 'cargo test watch'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo):** com `tools/dev.sh iniciar`, mandar 2 avisos e ligar o "não perturbe" na Central → os 2 somem na hora; `notify-send x` não aparece e o `notify-send` retorna id; desligar → próximos aparecem. Repouso de 30 s com DND ligado → `ctx_30s=0`.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/watch.rs`, `src/main.rs`.

---

### Tarefa 13: [opus] Privacidade: bloqueio e tela cheia — dois barramentos, protocolos

**Files:**
- Create: `src/privacy.rs`, `src/dbus/logind.rs`, `src/wayland/toplevel.rs`, `src/wayland/lock_layer.rs`, `protocols/cosmic-session-lock-layer-v1.xml` (o mesmo do spike), `tests/logind.rs`
- Modify: `src/wayland/mod.rs`, `src/main.rs`, `Cargo.toml` (+`cosmic-protocols`, +`wayland-scanner`, +`wayland-client`/`wayland-backend` nas versões que o sctk 0.20 usa)

**Interfaces:**
- Consumes: `Notification::is_private` (T2); `Bus`-style integração de fd (T5); `Ui`, `SurfaceKey` (T9); `CardView::redacted` (T8); `resultados-spike.txt: bloqueio_overlay_com_show_on_lock` (T1).
- Produces:
  ```rust
  // privacy.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Exposure { Full, Redacted }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct ScreenState { pub locked: bool, pub fullscreen: Option<bool> } // None = ainda desconhecido
  pub fn exposure(n: &Notification, s: &ScreenState) -> Exposure; // privado e (locked ou fullscreen==Some(true) ou fullscreen==None) → Redacted
  // dbus/logind.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum LogindEvent { Locked(bool), Sleep(bool) }
  pub struct Logind { /* canal do barramento de sistema */ }
  impl Logind {
      /// Barramento de sistema (respeita DBUS_SYSTEM_BUS_ADDRESS); sessão por Manager.GetSessionByPID(getpid()), senão "/org/freedesktop/login1/session/auto";
      /// match de PrepareForSleep e PropertiesChanged(LockedHint) só nesse caminho; lê LockedHint inicial.
      pub fn connect(handle: &LoopHandle<'static, State>) -> Result<(Logind, bool), dbus::Error>;
  }
  impl State { pub fn on_logind(&mut self, e: LogindEvent); }
  // wayland/toplevel.rs
  pub struct ToplevelWatch { /* ext_foreign_toplevel_list + zcosmic_toplevel_info_v1 (v2..=3) só enquanto ativo */ }
  impl ToplevelWatch {
      pub fn start(&mut self); pub fn stop(&mut self); pub fn is_active(&self) -> bool;
      pub fn fullscreen_focused(&self) -> Option<bool>;  // None até o 1º `done`; Some(algum toplevel com Activated e Fullscreen)
      pub fn available(&self) -> bool;                   // globais existem
  }
  // wayland/lock_layer.rs
  pub fn set_show_on_lock(mgr: &CosmicSessionLockLayerManagerV1, layer: &LayerSurface, on: bool); // + commit da superfície
  ```
- Regras:
  - `ToplevelWatch` **só existe com cartão de app (privado) visível**: `start` no primeiro, `stop` quando o último sai (`zcosmic_toplevel_info_v1.stop`, `ext_foreign_toplevel_list_v1.stop`, destruir handles). Motivo: o cosmic-comp manda `done` a todas as instâncias a cada mudança de qualquer janela (`toplevel_info.rs`, `last_dirty`), o que acordaria o daemon parado.
  - Cartão privado novo com o watch recém-iniciado: espera o primeiro `done` até **100 ms** (timer) antes do primeiro desenho; sem `done` no prazo → `fullscreen = None` → `Redacted` (falha fechada). Globais ausentes → `fullscreen = Some(false)` e log uma vez.
  - Mudança de `ScreenState` com cartões visíveis → re-`render` de cada um com a nova exposição.
  - `locked = true` (LockedHint): toda superfície (cartões e pílula) recebe `set_show_on_lock(true)`; `false` → `unset`. Sem o protocolo (global ausente ou portão da T1 negativo) → não marca: os cartões ficam invisíveis no bloqueio (o compositor os esconde).
  - `Sleep(b)` é repassado à T14 (`usb.on_sleep(b, now)`); até a T14 existir, só loga.
  - Falha do barramento de sistema → loga, `locked = false` fixo.
  - `Logind` também conecta em `--sem-tela` (é como `tests/logind.rs` o exercita).
- `tests/logind.rs`: um segundo `PrivateBus` faz papel de barramento de sistema; o teste registra `org.freedesktop.login1` e responde `GetSessionByPID` → `/org/freedesktop/login1/session/_31`, `Properties.Get(LockedHint)` → `false`; emite os sinais. O daemon roda com `--sem-tela`, `DBUS_SYSTEM_BUS_ADDRESS` e `NOTIFICACOES_DEBUG=1`, que imprime `logind: bloqueio=<b>` / `logind: suspensao=<b>` no stderr (lido pelo teste).

- [ ] **Passo 1: Testes que falham**

```rust
// privacy.rs
#[test] fn private_card_redacted_when_locked_or_fullscreen()
#[test] fn system_notices_never_redacted()          // pílula, compacto, category device* → Full mesmo bloqueado
#[test] fn unknown_fullscreen_fails_closed()        // fullscreen None → Redacted para privado
// toplevel.rs (estado puro de agregação)
#[test] fn fullscreen_needs_activated_and_fullscreen_same_window() // janela A fullscreen não ativa + B ativa → Some(false); A ativa+fullscreen → Some(true)
#[test] fn closed_window_removed_from_state()
// tests/logind.rs
#[test] fn initial_locked_hint_read()
#[test] fn locked_hint_change_reported()            // PropertiesChanged(LockedHint=true) no caminho da sessão → "logind: bloqueio=true"
#[test] fn other_session_changes_ignored()          // mesmo sinal em outro caminho → nada
#[test] fn prepare_for_sleep_reported()             // PrepareForSleep(true) e (false)
#[test] fn falls_back_to_auto_session_path()        // GetSessionByPID com erro → usa session/auto
```
- [ ] **Passo 2:** `H 'cargo test'` → FAIL. **Passo 3:** implementar. **Passo 4:** `H 'cargo test'` → PASS.
- [ ] **Passo 5 (ao vivo, tela cheia):** `tools/dev.sh iniciar`; abrir um vídeo em tela cheia (Brave, F11), `H 'sleep 3; notify-send -a Telegram "Fulano" "segredo"; sleep 1; grim docs/verificacao/t13-tela-cheia.png'` → cartão só com ícone, "Telegram" e "Nova notificação". Sair da tela cheia com o cartão na tela → conteúdo aparece.
- [ ] **Passo 6 (ao vivo, LockedHint manual, sem bloquear):** `H 'busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetLockedHint b true; notify-send -a Telegram "Fulano" "segredo"; sleep 1; grim docs/verificacao/t13-lockedhint.png; busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetLockedHint b false'` → cartão redigido.
- [ ] **Passo 7 (ao vivo, repouso com títulos mudando):** num terminal, `while true; do printf "\033]0;%s\007" $RANDOM; sleep 1; done`; sem cartão na tela, medir `ctx_30s` como na T9 → `0`. Com um cartão de app na tela o watch existe; depois que ele some, nova medição → `0`.
- [ ] **Passo 8: Deixar no working tree (não commitar)** — `src/privacy.rs`, `src/dbus/logind.rs`, `src/wayland/{toplevel,lock_layer,mod}.rs`, `src/main.rs`, `protocols/`, `tests/logind.rs`, `Cargo.toml`, `docs/verificacao/t13-*`.

---

### Tarefa 14: [opus] USB embutido — udev, agrupamento, suspensão

**Files:**
- Create: `src/usb/mod.rs` (monitor no calloop, mapa de conhecidos), `src/usb/classify.rs`, `src/usb/group.rs`
- Modify: `src/main.rs`, `Cargo.toml` (+`udev`)

**Interfaces:**
- Consumes: `UsbKind` (T2); `Config.usb`, `Config.usb_kinds` (T3); `NotifyArgs`, `notify_call`, `Bus::send` (T5); `Store::visible_with_sync_key` (T4); `LogindEvent::Sleep` (T13).
- Produces:
  ```rust
  // classify.rs (pura)
  #[derive(Debug, Clone, Default, PartialEq)] pub struct UsbProps {
      pub devpath: String, pub device_class: Option<String>, pub interfaces: Option<String>, pub model_db: Option<String>, pub vendor_db: Option<String>,
      pub product: Option<String>, pub manufacturer: Option<String>, pub vid_pid: Option<(u16, u16)>, pub mtp: bool, pub media_player: bool,
  }
  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)] pub struct InputHints { pub joystick: bool, pub keyboard: bool, pub mouse: bool }
  pub fn is_hub(p: &UsbProps) -> bool;                       // device_class "09" ou interfaces só ":09…"
  pub fn kind(p: &UsbProps, i: InputHints) -> UsbKind;
  pub fn display_name(p: &UsbProps) -> String;
  // group.rs (pura)
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Change { Added, Removed }
  #[derive(Debug, Clone, PartialEq)] pub struct Announcement { pub change: Change, pub names: Vec<String>, pub kinds: Vec<UsbKind> }
  pub const GROUP_WINDOW: Duration = Duration::from_millis(1500);
  pub struct Grouper { /* pendentes por Change, abertura da janela, nomes do último cartão por Change */ }
  impl Grouper {
      pub fn new() -> Grouper;
      pub fn push(&mut self, change: Change, name: String, kind: UsbKind, now: Instant) -> Instant; // prazo de flush
      /// `visible(change)`: o cartão de grupo dessa mudança ainda está na tela → soma aos nomes dele.
      pub fn flush(&mut self, now: Instant, visible: impl Fn(Change) -> bool) -> Vec<Announcement>;
  }
  pub fn title(a: &Announcement) -> String; pub fn body(a: &Announcement) -> String; pub fn icon_name(a: &Announcement) -> &'static str;
  pub fn sync_key(c: Change) -> &'static str;                // "notificacoes-usb-conectado" / "notificacoes-usb-desconectado"
  pub fn to_notify(a: &Announcement) -> NotifyArgs;
  // mod.rs
  pub const RESUME_QUIET: Duration = Duration::from_secs(5);
  pub struct Usb { /* MonitorSocket usb + input, conhecidos: HashMap<devpath,(nome,UsbKind)>, sleeping, quiet_until, grouper, input pendente */ }
  impl Usb {
      pub fn start(handle: &LoopHandle<'static, State>) -> std::io::Result<Usb>; // enumera os usb_device presentes em silêncio (só guarda nome/tipo)
      pub fn on_sleep(&mut self, sleeping: bool, now: Instant);
  }
  ```
- Classificação (`kind`, primeira regra que casa): `input.joystick` ou interfaces com `:ff5d01`/`:ff5d81`/`:ff47d0` → `Controle`; `:08` → `Armazenamento`; `mtp || media_player` ou `:0601` → `Celular`; `:01` → `Audio`; `input.keyboard` ou `:030101` → `Teclado`; `input.mouse` ou `:030102` → `Mouse`; senão `Generico`. `InputHints` vêm de eventos `input` (`ID_INPUT_JOYSTICK/KEYBOARD/MOUSE = 1`) cujo `devpath` começa com o `devpath` de um `usb_device` pendente na janela.
- Nome: `ID_MODEL_FROM_DATABASE` (com `ID_VENDOR_FROM_DATABASE` antes, se não estiver contido) → atributos `manufacturer` + `product` → `Dispositivo USB {vid:04x}:{pid:04x}`; underscores viram espaço; aparado; máx. 60 chars.
- Textos (`title`/`body`): 1 dispositivo → `"<Tipo> conectado"` / `"<Tipo> desconectado"` com os nomes de tipo das Global Constraints, corpo = nome; 2..=5 → `"{n} dispositivos conectados"`, corpo `"A e B"`, `"A, B e C"`; > 5 → corpo `"A, B… e mais {n-2}"`. Ícones: `Controle → input-gaming-symbolic`, `Armazenamento → drive-removable-media-symbolic`, `Celular → phone-symbolic`, `Audio → audio-headphones-symbolic`, `Teclado → input-keyboard-symbolic`, `Mouse → input-mouse-symbolic`, `Generico` ou tipos mistos → `media-removable-symbolic`.
- `to_notify`: `app_name "Dispositivos USB"`, `app_icon = icon_name`, `hints`: `x-estilo=compacto`, `category=device.added|device.removed`, `synchronous=sync_key`, e `x-tom=apagado` em `Removed`; `expire_timeout -1`; `replaces_id 0`. Enviado com `Bus::send(notify_call(..))` **esperando resposta** (a Central casa chamada e resposta).
- Regras de evento: ignora hubs; `remove` usa o nome/tipo guardado no `add` (ou na enumeração inicial); ignora `remove` de quem não conhecia; `usb = nao` ou tipo fora de `usb_avisa` → não anuncia (mas guarda). Suspensão: de `Sleep(true)` até `RESUME_QUIET` depois de `Sleep(false)` nada é anunciado (adds e removes), só o mapa é atualizado — comparação de `Instant` na hora do evento, sem timer. Timer só enquanto o `Grouper` tem pendentes.

- [ ] **Passo 1: Testes que falham**

```rust
// classify.rs
#[test] fn hub_is_ignored()                       // device_class "09" → true
#[test] fn kind_table()                           // ":080650:"→Armazenamento; ":010100:010200:"→Audio; ":ff5d01:"→Controle; ":030101:"→Teclado; ":030102:"→Mouse; mtp→Celular; ":ffff00:"→Generico
#[test] fn input_hints_refine_hid_gamepad()      // ":030000:" + joystick → Controle
#[test] fn name_prefers_database_then_descriptors_then_ids()
#[test] fn underscores_and_length_cleaned()
// group.rs
#[test] fn single_device_title_by_kind()          // [("8BitDo Pro 2",Controle)] Added → "Controle conectado" / "8BitDo Pro 2"
#[test] fn removed_single()                       // "Armazenamento USB desconectado"
#[test] fn three_devices_list_names()             // "3 dispositivos conectados" / "A, B e C"
#[test] fn two_devices_use_e()                    // "A e B"
#[test] fn eight_devices_two_names_and_count()    // "A, B… e mais 6"
#[test] fn mixed_kinds_use_generic_icon()         // icon_name == "media-removable-symbolic"
#[test] fn same_kind_group_uses_kind_icon()
#[test] fn events_within_window_become_one()      // push t, t+500ms, t+1400ms → flush(t+2900ms) = 1 Announcement com 3 nomes
#[test] fn added_and_removed_are_separate_groups()
#[test] fn event_while_group_visible_merges()     // flush com visible(Added)=true soma aos nomes do último cartão
#[test] fn group_not_visible_starts_fresh()
#[test] fn removed_notify_is_dimmed_and_compact() // to_notify: x-estilo compacto, x-tom apagado, category device.removed, synchronous
// mod.rs (estado puro de Usb sem socket, via construtor de teste)
#[test] fn present_at_start_silent_but_named_on_removal()
#[test] fn quiet_window_after_resume_suppresses_both()
#[test] fn disabled_kind_not_announced()
```
- [ ] **Passo 2:** `H 'cargo test usb'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo, com o usuário):** `tools/dev.sh iniciar`; plugar um pendrive → "Armazenamento USB conectado" com o nome, compacto, ícone do tipo; tirar → "… desconectado", ícone apagado; plugar um hub com 2–3 coisas → um cartão de grupo; plugar outro de mesmo tipo com o cartão na tela → soma; abrir a Central → os avisos estão no histórico. `systemctl suspend`, acordar → nenhum aviso em massa. Prints `docs/verificacao/t14-*.png`.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/usb/*`, `src/main.rs`, `Cargo.toml`, `docs/verificacao/t14-*`.

---

### Tarefa 15: [sonnet] Som — tema e pidfd

**Files:**
- Create: `src/sound.rs`, `tests/fixtures/sounds/` (`COSMIC/index.theme` com `Directories=stereo/alert stereo/notification` e sem `Inherits`; `COSMIC/stereo/notification/device-added.oga` e `message.oga` vazios; `freedesktop/index.theme` + `freedesktop/stereo/bell.oga`)
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Notification`, `Hints` (T2); `Config::{sound, player, sound_for_category}` (T3).
- Produces:
  ```rust
  pub const THEMES: [&str; 2] = ["COSMIC", "freedesktop"];
  pub fn sound_dirs() -> Vec<PathBuf>;   // $XDG_DATA_HOME/sounds, <XDG_DATA_DIRS>/sounds
  pub fn resolve(dirs: &[PathBuf], name: &str) -> Option<PathBuf>; // por tema em THEMES: Directories do index.theme (senão "stereo"), extensões oga, ogg, wav; depois tira o último "-sufixo" e repete
  #[derive(Debug, Clone, PartialEq)] pub enum SoundChoice { Silent, File(PathBuf), Name(String) }
  pub fn choose(n: &Notification, cfg: &Config, dnd: bool) -> SoundChoice; // Silent: !cfg.sound, dnd, suppress-sound, pílula; File: sound-file existente e regular; Name: sound-name, senão cfg.sound_for_category(category)
  pub struct Player { /* Rc<Cell<bool>> "tocando", compartilhado com o callback do pidfd */ }
  impl Player { pub fn new() -> Player; pub fn play<S: 'static>(&mut self, handle: &LoopHandle<'static, S>, player: &str, file: &Path) -> bool; } // false se ainda tocando; genérico para o teste usar um EventLoop<()>
  ```
- `play`: `Command::new(player).arg(file)` com stdin/stdout/stderr nulos; `rustix::process::pidfd_open` do filho num `Generic` READ; ao ficar legível, `wait()` e remove a fonte. Toca no `Show` (nunca no `Update` de pílula; cartões sim). `player` com espaço é dividido em programa + argumentos (para os testes).

- [ ] **Passo 1: Testes que falham**

```rust
#[test] fn resolves_in_cosmic_then_freedesktop()   // "device-added" → COSMIC/...; "bell" → freedesktop/...
#[test] fn name_fallback_drops_suffix()            // "device-added-usb" → device-added.oga
#[test] fn unknown_sound_is_none()
#[test] fn silence_rules()                         // cfg.sound=false; dnd; suppress-sound; pílula → Silent
#[test] fn sound_file_must_exist_and_be_regular()  // inexistente ou diretório → cai para Name/categoria
#[test] fn category_map_and_default()              // device.added → Name("device-added"); sem categoria → Name("message")
#[test] fn player_spawned_and_reaped_without_thread() // EventLoop de teste, player "true" → após dispatch: sem zumbi (/proc/<pid> some), Threads 1
#[test] fn sound_skipped_while_previous_playing()  // player "sleep 0.3": play → true; play de novo → false; após 400 ms + dispatch → true
```
- [ ] **Passo 2:** `H 'cargo test sound'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo):** `tools/dev.sh iniciar`; `notify-send x` toca `message`; `notify-send -h boolean:suppress-sound:true y` não toca; plugar USB toca `device-added`. `H 'pgrep -c pw-play'` alguns segundos depois → `0`.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/sound.rs`, `src/main.rs`, `tests/fixtures/sounds/`.

---

### Tarefa 16: [sonnet] Pílula OSD — porte do logi-osd

**Files:**
- Create: `src/render/glyphs.rs` (porte de `~/Projetos/logi-osd/src/osd/icons.rs`), `src/render/pill.rs` (porte de `~/Projetos/logi-osd/src/osd/render.rs`, texto via `Text` e não `ab_glyph`)
- Modify: `src/main.rs` (pílula: `SurfaceKey::Pill`), `src/wayland/mod.rs` (região de entrada vazia para a pílula)

**Interfaces:**
- Consumes: `Notification`, `Tom`, `Level` (T2); `Text` (T6); `shapes` (sombra, borda) e `palette` (T8); `Icons::named` (T7); `Ui` (T9); `Store` pílula (T4).
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Glyph { CapsLock, NumLock, Insert, FnLock, KeyboardBacklight, Battery, Charging, Switch }
  pub fn glyph_for(name: &str) -> Option<Glyph>; // logi-caps-lock, logi-num-lock, logi-insert, logi-fn-lock, logi-backlight, logi-battery, logi-battery-charging, logi-switch
  pub enum PillIcon<'a> { Drawn(Glyph), Themed(&'a Pixmap), None }
  pub enum PillValue<'a> { None, Text(&'a str), Dots { on: u8, total: u8 } }
  pub struct PillView<'a> { pub icon: PillIcon<'a>, pub label: &'a str, pub value: PillValue<'a>, pub tom: Option<Tom>, pub level: Level }
  pub fn pill_view<'a>(n: &'a Notification, themed: Option<&'a Pixmap>) -> PillView<'a>; // valor: pontos > corpo não vazio > value% > nada
  pub const HEIGHT: f32 = 38.0; pub const MIN_W: f32 = 260.0; pub const MAX_W: f32 = 420.0; pub const PAD_X: f32 = 18.0; pub const GAP: f32 = 12.0;
  pub const ICON: f32 = 18.0; pub const DOT: f32 = 9.0; pub const DOT_GAP: f32 = 6.0; pub const MARGIN_BOTTOM: i32 = 24;
  pub fn measure(text: &mut Text, v: &PillView, scale: u32) -> (u32, u32);   // tamanho da superfície (com enchimento de sombra)
  pub fn render(text: &mut Text, v: &PillView, theme: &Theme, scale: u32) -> Pixmap;
  ```
- Cores: ícone na cor do nível se `x-tom` for `atencao`/`critico`, senão destaque; valor: destaque (`normal`/ausente), texto a 80% (`apagado`), laranja/vermelho (`atencao`/`critico`); bolinhas acesas no destaque, apagadas no texto a 18%. Fundo, borda e sombra iguais aos do cartão, raio = altura/2. Superfície `Anchor::BOTTOM`, margem inferior `MARGIN_BOTTOM − pad_b`, sem saída fixa, região de entrada vazia. Sempre substitui; sem barra, sem som. `NOTIFICACOES_DEBUG=1` imprime `pilula: {ms} ms Notify→commit` (instante de chegada da mensagem até o `commit`).

- [ ] **Passo 1: Testes que falham** (portados e adaptados de `render.rs`/`icons.rs` do logi-osd)

```rust
#[test] fn width_is_at_least_260()
#[test] fn long_label_ellipsized_within_420()
#[test] fn dots_lit_and_unlit()                  // x-pontos 3 / total 8
#[test] fn value_priority()                      // pontos > body "Ativado" > value 42 → "42%"
#[test] fn apagado_dims_value()                  // pixel do valor com alfa ≈ 80% do texto
#[test] fn atencao_icon_and_value_orange()       // logi-battery + x-tom atencao → laranja no ícone e no valor
#[test] fn logi_names_map_to_glyphs()            // 8 nomes → Some; "logi-x" → None
#[test] fn every_glyph_draws_inside_its_box()    // porte do teste do logi-osd
#[test] fn pill_has_border_and_shadow()          // pixel logo abaixo da pílula com alfa > 0 (sombra); borda mais clara que o fundo
```
- [ ] **Passo 2:** `H 'cargo test -- pill glyphs'` → FAIL. **Passo 3:** implementar. **Passo 4:** PASS.
- [ ] **Passo 5 (ao vivo):** `H 'tools/dev.sh iniciar; for i in 1 2 3; do gdbus call --session --dest org.freedesktop.Notifications --object-path /org/freedesktop/Notifications --method org.freedesktop.Notifications.Notify logi-osd 0 logi-backlight Teclado "" "[]" "{\"x-estilo\": <\"osd\">, \"transient\": <true>, \"synchronous\": <\"logi-osd\">, \"x-pontos\": <$i>, \"x-pontos-total\": <7>}" 1500; sleep 0.3; done; grim docs/verificacao/t16-pilula.png'` → uma pílula só, no centro inferior, 3 bolinhas acesas, mesmo id nas três respostas; com `NOTIFICACOES_DEBUG=1` no log, `Notify→commit` < 30 ms.
- [ ] **Passo 6: Deixar no working tree (não commitar)** — `src/render/{glyphs,pill}.rs`, `src/main.rs`, `src/wayland/mod.rs`, `docs/verificacao/t16-pilula.png`.

---

### Tarefa 17: [haiku] Pacote Arch — molde do greeter

**Files:**
- Create: `packaging/arch/PKGBUILD`

**Interfaces:**
- Consumes: binário `notificacoes` (T5+), `data/config.exemplo` (T3), molde `~/Projetos/cosmic-greeter/packaging/arch/PKGBUILD` (rsync da árvore local, `_v3_flags`, target separado `target/makepkg`).
- Produces: pacote `cosmic-notifications-aenes`, `pkgver=0.1.0`, `pkgrel=1`, `arch=(x86_64)`, `license=(GPL-3.0-only)`, `depends=(dbus systemd-libs glibc gcc-libs pipewire cosmic-sound-theme cosmic-icon-theme noto-fonts noto-fonts-emoji ttf-opensans)`, `makedepends=(cargo rsync)`, `provides=("cosmic-notifications=1:1.10.0")`, `conflicts=(cosmic-notifications)`, `options=(!lto !debug)`. `package()`: `install -Dm0755 "$_target/release/notificacoes" "$pkgdir/usr/bin/cosmic-notifications"`, `install -Dm0644 data/config.exemplo "$pkgdir/usr/share/notificacoes/config.exemplo"`, `LICENSE` em `/usr/share/licenses/$pkgname/`. `check()`: `cargo test --frozen --lib`. Cabeçalho de comentários com instalação e o **roteiro de volta**:
  ```
  # Instalar:  cd packaging/arch && makepkg -f && sudo pacman -U cosmic-notifications-aenes-<versão>-x86_64.pkg.tar.zst   ("s" para remover cosmic-notifications)
  #            pkill -x cosmic-notifications    # a sessão religa o binário novo (e o painel pisca)
  # Voltar ao oficial:
  #   sudo pacman -U /var/cache/pacman/pkg/cosmic-notifications-1:1.10.0-1.1-x86_64_v3.pkg.tar.zst   ("s" para remover cosmic-notifications-aenes)
  #   pkill -x cosmic-notifications            # sem o cache: sudo pacman -S cosmic-notifications
  ```

- [ ] **Passo 1:** escrever o PKGBUILD. **Passo 2:** `H 'cd packaging/arch && makepkg -f'` → pacote gerado, `check()` verde. **Passo 3:** `H 'pacman -Qip packaging/arch/cosmic-notifications-aenes-0.1.0-1-x86_64.pkg.tar.zst | grep -E "Provê|Conflita|Provides|Conflicts"; pacman -Qlp packaging/arch/cosmic-notifications-aenes-0.1.0-1-x86_64.pkg.tar.zst'` → provides/conflicts certos, `/usr/bin/cosmic-notifications`.
- [ ] **Passo 4 (usuário instala):** o usuário roda os dois comandos de "Instalar". Verificar: `H 'busctl --user status org.freedesktop.Notifications | grep -E "^PID|^Comm"; pidof cosmic-notifications; journalctl --user -b --since "-2 min" | grep -i notificacoes | tail'` → dono é o `cosmic-notifications` novo, sem erros; `notify-send ok` aparece.
- [ ] **Passo 5: Deixar no working tree (não commitar)** — `packaging/arch/PKGBUILD` (o `.pkg.tar.zst`, `src/`, `pkg/` ficam fora do git: acrescentar ao `.gitignore`).

---

### Tarefa 18: [sonnet] Medição e aceite ao vivo — roteiro fechado

Com o usuário, com o pacote instalado (T17). Sem arquivo de código novo; números no resumo final.

**Files:** `docs/verificacao/t18-*.png` (prints).

- [ ] **Passo 1 (repouso, 3 estados):** com `M` = o script de medição abaixo, medir: (a) 30 s depois do login sem aviso; (b) 30 s depois de 6 avisos (2 com imagem, 1 pílula) terem sumido; (c) (b) com o loop de títulos da T13 rodando num terminal.

```sh
H 'pid=$(pidof cosmic-notifications);
   cs() { for t in /proc/$pid/task/*/status; do grep -E "^(voluntary|nonvoluntary)_ctxt_switches" $t; done | awk "{s+=\$2} END {print s}"; };
   a=$(cs); sleep 30; b=$(cs); echo "ctx_30s=$((b-a))"; grep -E "VmRSS|VmHWM|Threads" /proc/$pid/status'
```
Esperado: `ctx_30s=0` nos três; `Threads: 1`; `VmRSS ≤ 12288 kB`. Acima de 12 MB: se a T6 não está em modo reduzido, ligar o modo reduzido, reinstalar e medir de novo; ainda acima → relatar ao usuário com os números (não é bloqueio automático, é decisão dele).
- [ ] **Passo 2 (RSS depois de print 4K):** `notify-send` com `image-path` de um print da tela inteira do ultrawide; depois que some, medir → `VmRSS` volta a ≤ valor de (b) + 1 MB.
- [ ] **Passo 3 (visual, lado a lado):** `~/.local/bin/print-preview <arquivo>` e, ao lado, o mesmo arquivo pelo daemon (comando da T11) → `grim docs/verificacao/t18-lado-a-lado.png`. O usuário confirma "mesma família". Também: cartão comum normal/atenção/crítico, compacto, pílula, modo claro (alternar o tema) → prints.
- [ ] **Passo 4 (prints e captura):** com um cartão na tela, `grim` e o `cosmic-screenshot` → o cartão aparece nos dois (§2.10).
- [ ] **Passo 5 (funcional):** roteiro curto: rajada `for i in $(seq 50); do notify-send "n$i"; done` → 4 visíveis, Central com 50; DND; tela cheia; × / ação / expirar com `dbus-monitor`; USB; som; multi-monitor (mudar o foco de monitor entre dois avisos → a pilha fica no monitor do primeiro).
- [ ] **Passo 6 (crash):** `H 'pkill -SEGV -x cosmic-notifications; sleep 2; pidof cosmic-notifications; busctl --user status org.freedesktop.Notifications | grep Comm'` → religado pela sessão, com o nome; painel volta.
- [ ] **Passo 7: Relatório** ao usuário: tabela com os números (oficial 18,5 MB / 35 / 16 × novo), prints, divergências. Deixar no working tree (não commitar) — só `docs/verificacao/t18-*`.
- [ ] **Passo 8 (documentação, se o MCP `obsidian` estiver acessível):** nota `tipo: feature` em `20 Projetos/notificacoes/Evolucao/Feature - 2026-10-21 notificacoes.md`, ligada do hub do projeto, com as decisões (dbus, cosmic-text, protocolos) e as medições; fechar com o `brain-lint.py`. MCP fora → dizer "vault indisponível, documentação pendente".

---

### Tarefa 19: [opus] (Opcional) Compartilhamento de tela — investigação com portão

Design §2.10: suprimir a exibição durante compartilhamento de tela **só se a detecção for confiável**; senão, não fazer.

**Files:**
- Create (só se o portão aprovar): `src/screencast.rs`; testes conforme o método escolhido.
- Create (sempre): `docs/verificacao/t19-screencast.txt` (`chave = valor`: `metodo`, `brave_inicio_fim = sim|nao`, `obs_inicio_fim`, `kill9_cliente_detectado`, `custo_rss_kb`, `custo_threads`, `decisao = implementar|nao`).

- [ ] **Passo 1 (sonda, sem código no daemon):** observar com `dbus-monitor --session "interface=org.freedesktop.portal.ScreenCast" "interface=org.freedesktop.portal.Session" "interface=org.freedesktop.portal.Request"` durante: compartilhar tela no Brave (Meet de teste) e parar; OBS iniciar/parar captura; `kill -9` do cliente no meio. Registrar se cada início tem um fim observável (`Session.Closed` ou o dono do pedido sumindo — `NameOwnerChanged`).
- [ ] **Passo 2: Portão.** Critério de "confiável": início e fim observáveis nos 3 casos, sem polling e sem thread (descartados de saída: libpipewire — threads e vários MB; o applet `cosmic-ext-applet-privacy-indicator` instalado usa `pipewire 0.10`). Apresentar o `t19-screencast.txt` ao usuário. `decisao = nao` → tarefa termina aqui.
- [ ] **Passo 3 (se aprovado):** testes da máquina de estados (sessões abertas por remetente; `NameOwnerChanged` do remetente fecha as dele) → implementar `ScreenState.sharing` tratado como DND automático na exibição (aceita, não mostra, não toca). Medir de novo o repouso (`ctx_30s=0`).
- [ ] **Passo 4: Deixar no working tree (não commitar)** — `docs/verificacao/t19-screencast.txt` e, se houver, `src/screencast.rs`, `src/main.rs`.

---

## Autorrevisão (feita)

- **Cobertura do design:** §2.1 fontes → T5 (apps), T11 (print), T14 (USB), T16 (pílula), bateria = cartão com `x-tom` (T2/T8; produtor no plano irmão). §2.2 dicas → T2/T5; `x-estilo`/`x-tom` → T2/T8/T16. §2.3 formatos → T8 (cartão/compacto), T11 (imagem), T16 (pílula); barra e hover → T9/T10. §2.4 → T3/T8. §2.5 pilha, 4 visíveis, monitor → T4/T9. §2.6 som → T15. §2.7 DND → T4/T12. §2.8 config → T3/T12. §2.9 USB → T14. §2.10 prints → T1/T18; compartilhamento → T19. §2.11 privacidade → T13 (+ greeter no plano irmão). §3 → plano irmão. §5 anti-objetivos: leveza T9/T13/T18, sessão T5/T17/T18, perder notificações T4/T5, atraso T16, visual T8/T11/T18. §6 FD/pacote → T5/T17. §8.1–8.6 → T4, T16, T5/T10, T2, T13/T14, T14.
- **Tipos consistentes:** `Notification`, `Hints`, `HintValue`, `RawImage`, `Level`, `Tom`, `Estilo`, `UsbKind` (T2); `Effect`, `CloseReason`, `NewNotification`, `Store` (T4); `Call`, `NotifyArgs`, `notify_call`, `Bus` (T5); `Text`, `TextStyle` (T6); `Icons` (T7); `Format`, `Hit`, `Rect`, `CardView`, `CardLayout`, `ActionButton`, `SHADOW_PAD` (T8); `SurfaceKey`, `Ui`, `CardState`, `CardTiming` (T9) com os mesmos nomes nas tarefas que os consomem.
- **Proporção:** corpos só para regras que os testes não fixam (flags de nome, drenagem do libdbus, ordem de efeitos, fórmulas de layout, classificação USB, cópia de textos).

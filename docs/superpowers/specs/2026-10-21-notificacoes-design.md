# notificacoes — Design

> Daemon de notificações próprio para o COSMIC, no lugar do `cosmic-notifications`.
> Uma interface só para tudo: notificações dos apps, prints, avisos de USB e os avisos do logi-osd.
> Brainstorm: 2026-10-21. Mockups: `~/.cache/notificacoes-brainstorm/.superpowers/brainstorm/*/content/familia.html`.

## 1. Intenção e o porquê

Hoje há três interfaces de aviso diferentes na tela:

1. o `cosmic-notifications` (padrão do COSMIC), que o usuário acha feio;
2. o `print-preview` (`~/.local/bin/print-preview`, Python + GTK4 + gtk4-layer-shell), o cartão que aparece
   depois de um print feito pelo `print-recorte`. **O usuário gosta dele.** É a referência visual;
3. a pílula do `logi-osd` (`~/Projetos/logi-osd`), no centro inferior.

Faltava uma quarta: aviso de dispositivo USB conectado e desconectado, como no Windows. O usuário decidiu
**não** criar mais uma interface e unificar tudo num daemon só, com o visual do print-preview.

Esse daemon **substitui** o `cosmic-notifications`. Ele é instalado com o mesmo nome e no mesmo lugar, e a
sessão do COSMIC o inicia como se fosse o oficial. Funciona como uma "API de baixo custo": quem quer avisar
algo manda o conteúdo e o tipo de exibição pelo protocolo padrão de notificações do freedesktop, e o daemon
cuida de todo o resto.

## 2. Objetivos

### 2.1 Fontes de avisos

| Fonte | Como chega | Formato |
|---|---|---|
| Qualquer app (Brave, Telegram, `notify-send`…) | `org.freedesktop.Notifications.Notify` | cartão, ou imagem grande se houver imagem |
| Print (`print-recorte`) | `Notify` com `image-path` e ações "Editar" e "Abrir pasta" | imagem grande |
| USB conectado/desconectado | **embutido no daemon** (udev, no mesmo laço de eventos) | cartão compacto |
| logi-osd (Caps Lock, luz do teclado, DPI…) | `Notify` com `x-estilo: osd` | pílula no centro inferior |
| logi-osd (bateria baixa) | `Notify` com urgência alta/crítica | cartão no canto |

### 2.2 A "API": o protocolo padrão + dicas (hints)

Nenhum protocolo novo. Vale o freedesktop Notifications spec 1.2 inteiro (`Notify`, `CloseNotification`,
`GetCapabilities`, `GetServerInformation`, sinais `NotificationClosed`, `ActionInvoked`,
`ActivationToken`), mais as dicas abaixo, que decidem o formato:

| Dica | Efeito |
|---|---|
| `image-data` / `image-path` (ou `image_data`, `image_path`, `icon_data`) | formato **imagem grande** (miniatura até 320×180) |
| `value` (int 0–100) | valor mostrado na pílula de OSD (bolinhas ou número) |
| `category` | escolhe ícone e som (`device.added`, `device.removed`, `im.received`, `email.arrived`…) |
| `urgency` (0 baixa, 1 normal, 2 crítica) | cor dos detalhes e tempo na tela (2.4) |
| `transient` | não vai para o histórico da Central |
| `sound-name` / `sound-file` / `suppress-sound` | som (2.6) |
| `x-canonical-private-synchronous` / `synchronous` | substitui o aviso anterior com a mesma chave em vez de empilhar |
| **`x-estilo`** = `osd` \| `cartao` \| `imagem` | **nossa** dica, para forçar o formato quando o automático não basta |
| **`x-tom`** = `normal` \| `atencao` \| `critico` | **nossa** dica, para pedir a cor sem mudar a urgência do protocolo |

Regra de extensão: **um tipo novo de aviso é um estilo novo no daemon** (um renderizador a mais, escolhido
pelas dicas). Os produtores nunca desenham nada.

### 2.3 Formatos (decididos com mockups; molde = print-preview)

Todos usam o mesmo cartão: fundo `#1e1f24` a 94%, borda branca a 8%, cantos de 16 px, padding de 10 px,
sombra `0 8px 28px` preta a 45% e entrada com fade de 180 ms. O CSS do `print-preview` (seção `CSS`) é a
especificação visual de referência: tamanhos de fonte (13 px título semibold, 11–12 px texto), botões em
pílula (principal colorido, secundários translúcidos), botões redondos de 30 px e barra de 3 px.

- **Imagem grande:** largura de 340 px. Miniatura até 320×180 com cantos de 10 px, clicável (dispara a ação
  `default`). Embaixo: título, subtítulo e os botões em linha.
- **Cartão (comum):** largura de 340 px. Ícone do app de 40 px à esquerda, "App · agora" em cinza, título,
  texto (até ~3 linhas, com reticências) e × no canto. Os botões de ação do app ficam embaixo, alinhados à
  direita, com a ação principal colorida.
- **Cartão compacto (USB):** ícone pelo tipo (controle, pendrive/disco, celular, fone/áudio, mouse/teclado,
  genérico), título "Controle conectado" ou "Controle desconectado" e, no subtítulo, o nome do produto. Não
  tem botões. Quando desconectado, o ícone fica apagado.
- **Pílula OSD (decisão A2):** continua no **centro inferior**, como hoje no logi-osd, com o visual novo
  (mesmo fundo, borda e sombra, em pílula). Um aviso novo **substitui** o anterior e nunca empilha. Fica
  fora do histórico (sempre `transient`).

Todos os formatos, exceto a pílula, têm a **barra de luz decrescente** na base. **O mouse em cima congela a
barra**, e ela volta a correr quando o mouse sai.

### 2.4 Urgência: tempo e cor

**Nenhuma notificação fica presa na tela.** Todas somem sozinhas, inclusive as críticas. A urgência muda só
o tempo e a cor dos detalhes que hoje são azuis (barra, botão principal e destaque do ícone):

| Nível | Cor dos detalhes | Tempo na tela |
|---|---|---|
| normal (e baixa) | cor de destaque do tema do COSMIC (hoje azul) | 6 s |
| atenção | laranja (o mesmo do logi-osd) | 10 s |
| crítico | vermelho (o mesmo do logi-osd) | 15 s |

`expire_timeout` do app: respeitado quando for **maior** que 0 e **menor** que o padrão do nível.
`0` ("nunca expira") é tratado como o padrão do nível, porque nada é persistente. Os tempos ficam no arquivo
de configuração.

### 2.5 Pilha (decisão B1) e monitor

- Os cartões empilham **para cima** no canto inferior direito, com margem de 24 px como o print-preview e a
  mais nova embaixo. Cada uma tem a própria barra.
- Ficam no máximo **4 visíveis**. A quinta empurra a mais antiga para fora da tela; ela continua no
  histórico da Central.
- **Monitor:** o selecionado. O daemon não fixa a saída do layer-shell e deixa o cosmic-comp pôr o aviso no
  monitor ativo.

### 2.6 Som

- Usa o tema de som do COSMIC (`/usr/share/sounds/COSMIC/`, pacote `cosmic-sound-theme`), com o
  `freedesktop` como reserva. A escolha do arquivo segue a spec de temas de som (`sound-name` →
  arquivo `.oga`).
- O som padrão por categoria vem da configuração: `device.added` → `device-added`, `device.removed` →
  `device-removed`, bateria → `battery-low`, o resto → um som de notificação.
- **Configurável:** liga/desliga geral, liga/desliga por categoria e troca do som por categoria, no
  arquivo de configuração (2.8). Respeita `suppress-sound`; pílulas OSD não tocam som.
- A reprodução não pode pesar no daemon parado. Uma opção é disparar `pw-play` como processo filho só na
  hora; isso fica a cargo de quem construir.

### 2.7 Não perturbe

- Fonte da verdade: a chave `do_not_disturb` da config `com.system76.CosmicNotifications` v1, que a
  Central já grava (`quick-settings/src/services.rs`, `DO_NOT_DISTURB`).
- Ligado: **nada aparece e nada toca.** As notificações continuam sendo aceitas pelo D-Bus (têm id, sinais
  normais) e se acumulam no histórico da Central.
- Pílulas OSD também ficam ocultas.

### 2.8 Configuração

Arquivo de texto opcional (como `~/.config/logi-osd/config`): tempos por nível, máximo de visíveis, sons por
categoria, liga/desliga de som e quais classes de USB avisam. Se o arquivo não existir, valem os padrões
deste documento. Sem tela de configuração na v1.

### 2.9 USB (embutido)

- O daemon escuta `udev` (subsistema `usb`, `DEVTYPE=usb_device`) no mesmo laço de eventos. Não faz polling
  e não cria processo extra.
- Avisa uma vez por **dispositivo físico**, não por interface: um controle que aparece como teclado,
  mouse e joystick gera **um** aviso só.
- Ignora hubs (`bDeviceClass=09`) e dispositivos que já estavam conectados quando o daemon iniciou (boot,
  login, reinício do daemon). Ao voltar da suspensão, não deve avisar em massa os dispositivos que
  "reconectam".
- O nome legível vem de `ID_MODEL_FROM_DATABASE` / `ID_VENDOR_FROM_DATABASE` ou dos descritores `product`
  e `manufacturer`, e é guardado ao conectar, porque na desconexão o dispositivo já sumiu.
- Tipo e ícone pela classe ou interface USB e pelas propriedades udev (`ID_INPUT_JOYSTICK`,
  `ID_INPUT_KEYBOARD`, `ID_MEDIA_PLAYER`, `ID_USB_INTERFACES` com mass storage ou áudio…).
- Os avisos de USB vão para o histórico da Central (não são `transient`).
- **Vários ao mesmo tempo (decisão do usuário):** dispositivos do **mesmo tipo de aviso** (conectados ou
  desconectados) que chegam juntos viram **um cartão só**:
  - de 2 a 5 dispositivos: o título é "3 dispositivos conectados" e o corpo lista os nomes ("USB X, USB Y e
    USB Z");
  - mais de 5: o corpo mostra os primeiros nomes e o resto como contagem ("USB X, USB Y… e mais 6"),
    seguindo o exemplo do usuário (dois nomes + contagem);
  - o ícone é o do tipo, se todos forem do mesmo tipo, ou o genérico de USB, se forem mistos;
  - a janela de agrupamento é uma escolha do Claude (seção 8).
- Um dispositivo sozinho continua com o cartão compacto individual (2.3).

### 2.10 Prints e compartilhamento de tela

- **Os avisos precisam aparecer nos prints** (`grim` / screencopy e o `cosmic-screenshot`). Isso deve
  ser verificado de verdade antes da entrega.
- **Opcional, por último:** enquanto houver compartilhamento de tela ativo (portal ScreenCast), suprimir a
  exibição (comportamento igual ao "não perturbe", mas automático). O Wayland não permite esconder uma
  superfície só do compartilhamento e mantê-la no print. **Se a detecção não for confiável, não fazer**
  (decisão do usuário).

### 2.11 Privacidade: tela bloqueada e tela cheia (decisão do usuário)

- Com a **sessão bloqueada** ou com uma **janela em tela cheia** em foco (jogo, vídeo), o aviso de app
  continua aparecendo, mas **sem conteúdo**. Mostra só o ícone e o nome do app (por exemplo, "Telegram" com
  o texto "Nova notificação"), sem título, corpo, miniatura nem botões.
- O conteúdo completo continua no histórico da Central, para ver depois.
- **Escolha do Claude:** avisos do sistema que não são privados (USB, pílula OSD, bateria) aparecem normais,
  porque o conteúdo deles não expõe nada. Se ficar errado, é só mudar.
- **Detecção:** bloqueio pelo `LockedHint` da sessão no logind (barramento de sistema, que já é preciso
  para o `PrepareForSleep`). Tela cheia pelo protocolo de informações de janelas do cosmic-comp (estado
  `fullscreen` da janela ativa), escutando eventos, sem polling.
- **Verificar no começo:** se o cosmic-comp desenha superfícies layer-shell `Overlay` por cima da tela de
  bloqueio (o `cosmic-greeter-aenes`). Se não desenhar, na tela bloqueada nada aparece. O usuário
  precisa ser avisado, e mostrar avisos na própria tela de bloqueio vira assunto do fork do greeter, fora
  deste projeto.

## 3. Mudanças em outros projetos (fazem parte da entrega)

1. **`print-recorte`** (`~/.local/bin`): em vez de chamar o `print-preview`, manda `Notify` com
   `image-path`, título "Captura copiada", subtítulo "L×A · salvo em Screenshots" e as ações `default`/
   "Editar" (abre o Satty com os mesmos argumentos de hoje) e "Abrir pasta". O `print-preview` é
   aposentado. Quem trata o clique é o próprio `print-recorte`, ou um ajudante dele, escutando
   `ActionInvoked`. Hoje o script termina logo depois de capturar, então ele vai precisar ficar vivo até a
   notificação fechar.
2. **logi-osd:** para de desenhar (sai a camada `osd/` de Wayland e renderização) e passa a mandar `Notify`
   com `x-estilo: osd`, `value`, `transient` e uma chave `synchronous` única para substituir o aviso
   anterior. A bateria baixa vira cartão com `x-tom` atenção/crítico. As regras de comportamento do spec
   do logi-osd continuam valendo (avisa uma vez a 15% e a 5%, nunca empilha, não avisa ao desconectar).
3. **Central** (`quick-settings`): o histórico ignora as notificações com `transient`. Hoje ela também
   invoca ações pelo socket do painel (`com.system76.NotificationsApplet.InvokeAction`, via
   `COSMIC_NOTIFICATIONS`). O daemon novo precisa oferecer um caminho equivalente para a Central acionar
   uma ação a partir do histórico.

## 4. Não-objetivos

- Sem notificações persistentes ou residentes.
- Sem tela de configuração (só o arquivo).
- Sem central de notificações própria: o histórico continua sendo o da Central.
- Sem suporte a outros desktops além do COSMIC.
- Sem markup rico além do subconjunto da spec (`<b>`, `<i>`, `<u>`, `<a>` como texto e `<img>` ignorado).
  O que não for suportado vira texto puro.
- Sem rede e sem telemetria.

## 5. Anti-objetivos (fracasso mesmo funcionando)

- **Pesar.** Referência medida em 2026-10-21, 30 s parado: `cosmic-notifications` 18,5 MB RSS, 35
  threads e 16 acordadas; `logi-osd` 4,2 MB, 3 threads e 0 acordadas. Meta: **poucos MB, poucas threads e
  zero acordadas parado.** Nada de polling, nada de timer rodando sem aviso na tela, e nada de toolkit
  completo (libcosmic/iced/GTK). O usuário vai medir.
- **Quebrar a sessão.** Se o daemon cair, a sessão o reinicia. Ele não pode entrar em loop de crash nem
  deixar o painel ou a Central sem notificações. A volta ao oficial tem de ser trivial (reinstalar o pacote
  `cosmic-notifications`).
- **Perder notificações.** Toda chamada `Notify` recebe id e gera os sinais corretos, mesmo com o "não
  perturbe" ligado ou a pilha cheia. O histórico da Central não pode perder nada que hoje recebe.
- **Atraso visível** na pílula OSD em relação à pílula atual do logi-osd.
- **Visual diferente do print-preview.** Se o usuário olhar um aviso comum e um print lado a lado, tem de
  parecer da mesma família.

## 6. Restrições e fatos levantados no sistema

- **Sistema:** CachyOS, kernel 7.2, COSMIC 1.10 (cosmic-comp, Wayland), 3 monitores (o principal é o LG
  ultrawide, DP-3).
- **Como a sessão inicia o daemon:** o `cosmic-session` executa `cosmic-notifications` pelo nome e passa
  `DAEMON_NOTIFICATIONS_FD` (socket com o painel). O painel entrega o outro lado aos applets marcados
  `X-NotificationsApplet` (variável `COSMIC_NOTIFICATIONS`). Hoje nenhum applet desses está no painel do
  usuário. O daemon novo precisa aceitar esse FD sem quebrar o painel; o protocolo deve ser conferido no
  código do upstream `pop-os/cosmic-notifications` 1.10.
- **Instalação:** como no fork do greeter (`cosmic-greeter-aenes`), um pacote Arch próprio
  (`provides`/`conflicts` `cosmic-notifications`, PKGBUILD em `packaging/arch/`), x86-64-v3. Volta:
  `pacman -U` do pacote oficial 1:1.10.0 no cache.
- **Base de código para reaproveitar:** o logi-osd (Rust edition 2024, `smithay-client-toolkit` 0.20 com
  calloop, `tiny-skia`, `ab_glyph` com fonte via mmap, `udev`, `rustix`). Ele já tem layer-shell, desenho
  por software, tema do COSMIC (`osd/theme.rs`: destaque, claro/escuro, `WARNING`/`CRITICAL`), ícones e
  região de entrada. Perfil release: `opt-level=3`, `panic="abort"`, `lto="thin"`, `codegen-units=1`,
  `strip`.
- **Novidades em relação ao logi-osd:** D-Bus (servidor `org.freedesktop.Notifications`), texto com quebra
  de linha e reticências, decodificação de imagem (PNG/JPEG/`image-data` cru), ícones de app pelo tema e
  pelo `.desktop`, entrada de ponteiro (hover, clique em botões, ×), animação de entrada e da barra (só
  enquanto houver aviso na tela), várias superfícies (pilha + pílula).
- **Ícones de app:** a Central já resolve o ícone pelo `.desktop` (`quick-settings/src/notifications.rs`,
  `app_icon`/`desktop_icon`), e essa lógica serve de referência.
- **Tema:** claro/escuro e cor de destaque lidos da config do COSMIC, como no logi-osd. O fundo escuro
  `#1e1f24` do print-preview é a referência do modo escuro; o claro fica a cargo de quem construir, seguindo
  o tema.
- **Sons disponíveis:** `cosmic-sound-theme` 1.10 (`stereo/notification/device-added.oga`,
  `device-removed.oga`, `battery-caution.oga`, `complete.oga`…; `stereo/alert/battery-low.oga`…), `pw-play`
  e `canberra-gtk-play` instalados.
- **Ambiente de desenvolvimento:** sandbox Flatpak. Build, execução e prints via `flatpak-spawn --host`.

## 7. Ordem sugerida

1. Daemon: protocolo D-Bus + cartão comum + pilha + barra/hover + urgência + "não perturbe" (já
   substituindo o oficial em teste).
2. Formato imagem + migração do `print-recorte`.
3. USB embutido + som.
4. Pílula OSD + migração do logi-osd.
5. Ajustes na Central (`transient`, acionar ações).
6. (Opcional) supressão durante compartilhamento de tela.

## 8. Escolhas do Claude (não decididas pelo usuário — conferir)

Resolvidas depois da revisão do documento:

1. **Sumir por tempo não fecha a notificação no protocolo.** Igual ao upstream 1.10: ao expirar, ou ao
   sair da pilha por falta de espaço, o aviso só sai da tela. O daemon guarda a notificação numa lista
   oculta (até ~200) e **não** emite `NotificationClosed`. O motivo: a Central apaga do histórico tudo o
   que fecha com motivo diferente de 2, e esse sinal esvaziaria o histórico em segundos. Assim as ações
   continuam acionáveis pelo histórico. `NotificationClosed` só sai em ×, clique ou `CloseNotification`.
   Exceção: avisos `transient` (pílulas OSD) emitem o motivo 1 ao expirar e não ficam na lista. Como o
   `notify-send --wait` não termina com um aviso que só expirou, o `print-recorte` usa um ajudante próprio
   com um prazo limite (por exemplo, 10 min) para escutar `ActionInvoked`.
2. **API da pílula OSD:** o ícone vai em `app_icon`, com um vocabulário de nomes próprio para os ícones
   desenhados do logi-osd (`logi-caps-lock`, `logi-num-lock`, `logi-backlight`, `logi-battery`,
   `logi-battery-charging`, `logi-switch`…) e reserva pelo tema de ícones. A dica `value` (0–100) vale
   para valores percentuais. As dicas novas são **`x-pontos`** (aceso) e **`x-pontos-total`** (total), para
   as bolinhas com total por dispositivo. O texto do valor ("Ativado", "Canal 1", "42%") vai no `body`, e o
   tom vem de `x-tom` (incluindo `apagado` para "Desativado").
3. **Ações a partir da Central:** um método no barramento de sessão, exposto pelo próprio daemon
   (`com.system76.NotificationsApplet.InvokeAction(id, action)` no mesmo nome de barramento). Não passa
   pelo socket do painel. O FD `DAEMON_NOTIFICATIONS_FD` é aceito só o suficiente para não quebrar o
   painel. Fato apurado: hoje a Central não está marcada `X-NotificationsApplet`, então acionar ação pelo
   histórico não funciona hoje (falha silenciosa). Este projeto conserta isso.
4. **Markup:** convertido para texto puro, como a Central faz. Sem negrito ou itálico na v1, para não
   carregar mais fontes.
5. **USB, volta da suspensão:** detectada pelo sinal `PrepareForSleep(false)` do logind, seguido de uma
   janela de alguns segundos sem avisos de "conectado". Isso precisa de uma conexão com o barramento de
   sistema. O agrupamento de vários dispositivos foi decidido pelo usuário (2.9).
6. **USB, janela de agrupamento:** ~1,5 s depois do primeiro evento. Se o cartão de grupo ainda estiver na
   tela, um evento do mesmo tipo é **somado a ele** (o cartão é atualizado e a barra reinicia), em vez de
   abrir outro.

## 9. Fica a cargo de quem construir

Estrutura interna do código, nome do pacote e do repositório (sugestão: `notificacoes`, pacote
`cosmic-notifications-aenes`), formato exato do arquivo de configuração, curvas e durações de animação
(partindo dos 180 ms do print-preview), como tocar o som, ícones desenhados versus do tema, e o visual do modo
claro.

## 10. Mudanças decididas durante a execução

- **2026-10-21, tela de bloqueio (decisão do usuário, revoga parte do §2.11):** depois do teste ao vivo
  (nada apareceu sobre a tela de bloqueio), o usuário pediu para **não mexer no greeter**. Na tela
  bloqueada, nenhum aviso aparece; tudo continua indo para o histórico da Central. A privacidade de
  "só o nome do app" vale apenas para a **tela cheia**.

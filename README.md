# Fast Notes

Notas rápidas para Linux/Wayland em Rust puro: sem GTK, sem Qt, sem GPU.
Abre em ~70 ms, usa ~15–25 MB de RAM (dois quadros da janela + fontes) e
0 % de CPU parado. Cada nota é um arquivo `.md` UTF-8 puro, renderizado ao
vivo como no Notion.

- **Wayland direto** (`smithay-client-toolkit`), renderização por software em `wl_shm`.
- **Texto** com `cosmic-text` (quebra de linha, seleção, emoji colorido, qualquer script),
  na fonte **Inter** (variável, incluída em `fonts/`, licença OFL).
- **Markdown ao vivo**: títulos, negrito/itálico/riscado/código, listas, checkboxes
  clicáveis, citações, separadores, tabelas alinhadas, blocos de código, links,
  toggles recolhíveis, colunas (até 4), imagens inline (PNG/JPEG/WebP) e menu `/`.
  Os marcadores (`**`, `#`, URL…) só aparecem na linha que está sendo editada.
- **Cor em qualquer trecho** com roda de cores (`Ctrl+Shift+C`): `{#ff8800 texto}`,
  até dentro de uma palavra. Sem bloquinhos: qualquer intervalo de caracteres.
- **Tema**: fundo `#000000`, texto `#ffffff` (tons são branco com transparência).
- **Abas** na barra superior; reabertas na próxima sessão.
- **Acentos e dead keys** via compose do xkbcommon (´ + a = á, ~ + o = õ, AltGr etc.).
- **Auto-save** 1,5 s depois da última tecla, ao perder o foco, ao trocar/fechar aba
  e ao sair (escrita atômica).
- **Clipboard** do sistema (Ctrl+C/X/V) e seleção primária (botão do meio).
- **Instância única**: chamar `fastnotes` de novo só traz a janela à frente.
- **Undo/redo** por palavra (Ctrl+Z / Ctrl+Shift+Z ou Ctrl+Y).

## Instalar

Pela loja (Discover, GNOME Software ou qualquer loja com Flathub):

```sh
flatpak install flathub io.github.tevoetals.fastnotes
```

A partir do código, otimizado para a sua CPU:

```sh
./install.sh            # compila e instala em ~/.local/bin/fastnotes + menu
./install.sh --uninstall
```

Atalho global no KDE: Configurações do Sistema → Atalhos → Adicionar novo →
Comando ou script: `fastnotes`.

## Tutorial rápido

### Escrever e formatar

Selecione um trecho (Shift+setas ou arrastando o mouse) e aperte o atalho.
Sem seleção, o atalho insere os marcadores e deixa o cursor no meio. Apertar
de novo sobre um trecho já formatado remove a formatação.

| Atalho | Resultado | Markdown gerado |
|---|---|---|
| `Ctrl+B` | **negrito** | `**texto**` |
| `Ctrl+I` | *itálico* | `*texto*` |
| `Ctrl+Shift+S` | ~~riscado~~ | `~~texto~~` |
| `Ctrl+E` | `código` | `` `texto` `` |
| `Ctrl+K` | link | `[texto](https://)` (edite a URL) |
| `Ctrl+Shift+C` | cor: abre a roda de cores (mouse), clique aplica, **0** remove | `{#ff8800 texto}` |

Os marcadores ficam visíveis só na linha onde está o cursor; nas outras
somem e o texto aparece formatado. Para ver o Markdown de uma linha, basta
levar o cursor até ela.

### Blocos (linha inteira)

Funcionam na linha do cursor ou em todas as linhas selecionadas. Apertar de
novo o mesmo atalho volta para texto normal.

| Atalho | Bloco | Markdown |
|---|---|---|
| `Ctrl+Shift+1` … `Ctrl+Shift+6` | Título 1 … 6 | `# `, `## `, … |
| `Ctrl+Shift+0` | Texto normal (remove prefixo) | |
| `Ctrl+Shift+8` | Lista com pontos | `- ` |
| `Ctrl+Shift+7` | Lista numerada | `1. ` (Enter continua a numeração) |
| `Ctrl+Shift+9` | Checkbox | `- [ ] ` |
| `Ctrl+Enter` | Marca/desmarca o checkbox da linha (cria um se não houver); num toggle, abre/fecha | `- [x] ` |
| `Ctrl+Shift+Q` | Citação | `> ` |
| `Ctrl+Shift+T` | Tabela 3×2 vazia abaixo da linha | `\| Coluna 1 \| …` |

Também dá para digitar direto: `- ` vira ponto, `1. ` numera, `- [ ] ` vira
checkbox (clique nele para marcar), `> ` cita, `---` numa linha vira
separador, três crases abrem/fecham um bloco de código.

- **Enter** no fim de um item continua a lista (ponto, número seguinte, checkbox).
- **Enter** num item vazio encerra a lista.
- **Tab / Shift+Tab** fora de tabela: indenta/desindenta (sub-itens).

### Menu `/`

Numa linha vazia (ou só com espaços) digite `/`: abre o menu de blocos. Continue
digitando para filtrar (`/tab`, `/col`, `/img`), `↑↓` escolhe, `Enter` aplica,
`Esc` fecha. Itens: Título 1–3, Texto normal, Lista, Lista numerada, Checkbox,
Toggle, Citação, Divisor, Tabela, 2/3/4 colunas, Imagem, Bloco de código.

### Toggles (conteúdo recolhível)

Uma linha começando com `▾ ` (aberto) ou `▸ ` (fechado); o conteúdo são as
linhas **recuadas** abaixo dela (2 espaços ou mais). Crie com `/toggle`.
`Enter` no fim da linha do toggle já entra recuado. Clique no triângulo ou use
`Ctrl+Enter` na linha para abrir/fechar. Fechado, as linhas recuadas somem e o
cursor pula por cima delas. No arquivo fica exatamente assim (legível em
qualquer editor):

```markdown
▸ Título do toggle
  primeira linha escondida
  segunda linha escondida
```

### Colunas (até 4)

Crie com `/2 colunas`, `/3 colunas` ou `/4 colunas`, ou digite:

```markdown
:::
conteúdo da coluna 1
(quantas linhas quiser: listas, títulos, negrito…)
|||
conteúdo da coluna 2
:::
```

As colunas são desenhadas lado a lado com largura igual; a altura do bloco é a
da coluna mais alta. Você clica, arrasta e edita dentro de cada coluna; `↑`/`↓`
percorrem as linhas de uma coluna e passam para a seguinte. As linhas `:::` e
`|||` ficam invisíveis. Tabelas dentro de colunas aparecem sem a grade.

### Imagens

- **Colar** (Ctrl+V) uma imagem do clipboard ou **arrastar** um arquivo
  PNG/JPEG/WebP para a janela: a imagem é convertida para WebP (sem perdas,
  no máximo 1600 px de largura), guardada em `~/.local/share/fastnotes/images/`
  e inserida **no ponto do cursor**, inclusive no meio de uma frase.
- A sintaxe é a de Markdown, `![alt](images/nome.webp)`, e pode aparecer
  várias vezes na mesma linha: texto, imagem, texto, imagem. O texto fica
  centrado verticalmente ao lado da imagem.
- **Redimensionar com o mouse:** passe o mouse num canto da imagem (o cursor
  vira seta diagonal) e arraste; para fora aumenta, para dentro diminui, até a
  largura da janela ou o mínimo de 24 px, mantendo a proporção. A largura é
  gravada no arquivo como `![alt](caminho =300x)` (extensão usada por vários
  editores Markdown); sem `=Wx`, a imagem usa o tamanho natural limitado à
  largura disponível.
- O marcador só aparece, apagado, na linha do cursor; PNG, JPEG e WebP são
  decodificados em Rust puro. `/imagem` insere um marcador vazio para você
  preencher o caminho.

### Tabelas

Crie com `Ctrl+Shift+T` ou digite linhas começando com `|`:

```markdown
| Piloto | Equipe | Status |
|:-------|:------:|-------:|
| Norris | McLaren | Presente |
```

Na tela a tabela vira uma grade de células proporcionais, no estilo das
referências de design de tabelas (Few, Ström, NN/g, Tufte):

- **Largura da coluna pelo conteúdo** (célula mais larga + 12 px de cada lado),
  fonte do corpo com algarismos tabulares, sem monoespaçada.
- **Números à direita, texto à esquerda**, e o cabeçalho segue o alinhamento da
  coluna. Uma coluna é numérica quando todas as células preenchidas são números
  (aceita `R$`, `%`, `1.234,56`, sinais). `:---`, `:---:` e `---:` forçam
  esquerda, centro e direita.
- **Cabeçalho distinto** (semibold) com régua mais forte embaixo; entre as
  linhas, réguas suaves; sem linhas verticais nem zebra (menos tinta, mais
  dado).
- Se a tabela não cabe na medida, a fonte da tabela diminui (até ~55 %).
- `Tab` / `Shift+Tab` pulam entre células (o separador `|---|` é pulado);
  `Enter` cria uma linha nova (no cabeçalho, depois do separador); no fim da
  última célula, `Tab` também cria uma linha. Clique numa célula para editar.
- Formatação dentro das células funciona (`**negrito**`, `*itálico*`, cores);
  os marcadores só aparecem na célula com o cursor.
- No arquivo, as colunas continuam alinhadas por espaços (largura visível), o
  que mantém a tabela legível em qualquer editor.

### Cores

Selecione o trecho e aperte `Ctrl+Shift+C`: abre uma **roda de cores**. Passe
o mouse pela roda (matiz e saturação) e pela barra ao lado (brilho): a seleção
já mostra a cor como prévia. Clique (ou Enter) para aplicar, `0` remove a cor
da seleção, `Esc` cancela. Sem seleção, a cor vale para o que você digitar em
seguida.

No arquivo fica `{#rrggbb texto}`; os marcadores nunca aparecem na tela, só o
texto colorido. Pode envolver parte de uma palavra (`pal{#6ea8ff avr}a`) e
conter outros estilos (`{#55e07a **forte**}`). Nomes também são aceitos ao
digitar (`{vermelho texto}`, `{green text}`).

### Emoji e símbolos

- `Ctrl+.` (ou `Ctrl+Shift+E`) abre o **seletor de emoji**: 1 906 emoji com
  nomes e palavras-chave em português e inglês (dados do Unicode/CLDR). Digite
  para filtrar (`cora`, `foguete`, `bandeira`, `rocket`), setas movem, `Enter`
  insere, `Esc` fecha; o mouse também seleciona e a roda rola.
- `Ctrl+,` (ou `Ctrl+Shift+I`) abre o **seletor de símbolos** ("alt codes"):
  setas, marcas (✓ ✗ ☐ ☑), formas, matemática, moedas, pontuação tipográfica,
  caixas e diversos, com busca por nome ou grupo (`seta`, `check`, `grau`,
  `caixa`).

### Movimento e edição rápida

**Enter × Shift+Enter (meio espaço).** `Enter` quebra a linha com a
distância normal (entrelinha 1.5). `Shift+Enter` faz uma **quebra curta**: a
linha seguinte fica 30 % mais perto (distância 1.05 em em vez de 1.5, as
letras quase se tocam), e a linha anterior não se move. Funciona em texto, listas (continua o item),
títulos, citações, toggles e depois de `---`; em tabela cria uma linha nova.
No arquivo é um `\` no fim da linha (quebra dura do CommonMark), que só
aparece na linha do cursor. Assim você agrupa por proximidade (Gestalt): quebra
curta para a mesma família, `Enter` para a família vizinha, linha em branco
para outra família.

| Tecla | Ação |
|---|---|
| `↑` na primeira linha / `↓` na última | Vai ao início / fim do texto |
| `Home` | Alterna entre o primeiro caractere da linha e a coluna 0; `End` vai ao fim |
| `Ctrl+↑` / `Ctrl+↓` | Início / fim do parágrafo |
| `Ctrl+←` / `Ctrl+→` | Palavra anterior / seguinte (com `Shift`, seleciona) |
| `Ctrl+Home` / `Ctrl+End` | Início / fim da nota |
| `PgUp` / `PgDn` | Uma tela acima / abaixo |
| `Shift` + qualquer movimento | Seleciona |
| `Alt+↑` / `Alt+↓` (ou `Ctrl+Shift+↑/↓`) | Move a linha para cima / baixo |
| `Ctrl+D` | Duplica a linha |
| `Ctrl+Shift+K` | Apaga a linha inteira |
| `Ctrl+Backspace` / `Ctrl+Delete` | Apaga a palavra anterior / seguinte |
| `Shift+Enter` | Quebra de linha simples (não continua listas) |
| `Esc` | Limpa a seleção; sem seleção, fecha o app |
| Duplo clique / triplo clique | Seleciona palavra / linha |

### Notas e abas

- `Ctrl+Shift+N` duplica a nota atual numa aba nova (vira outro arquivo ao
  salvar), para começar uma variação sem mexer na original.

| Atalho | Ação |
|---|---|
| `Ctrl+N` / `Ctrl+T` | Nova nota (em nova aba) |
| `Ctrl+L` (ou `Ctrl+O`, `Ctrl+K`) | Lista/busca de notas (digite para filtrar, ↑↓ Enter) |
| `Ctrl+W` | Fechar aba (botão do meio na aba também fecha) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+PgDn` / `Ctrl+PgUp` | Próxima / aba anterior |
| `Ctrl+1` … `Ctrl+9` | Ir para a aba N |
| `Ctrl+S` | Salvar agora (o auto-save já faz isso sozinho) |
| `Ctrl+Shift+D` | Mover nota para a lixeira (`trash/`, recuperável) |
| `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y` | Desfazer / refazer |
| `Ctrl+A/C/X/V` | Selecionar tudo, copiar, recortar, colar |
| `Ctrl+←/→`, `Ctrl+Backspace/Delete` | Mover/apagar por palavra |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | Zoom do texto |
| `Esc` (sem seleção), `Ctrl+Q` | Sair (salva tudo antes) |

Mouse: clique, arraste, duplo clique (palavra), triplo (linha), roda para rolar,
clique no checkbox alterna, botão do meio cola a seleção primária.

O título da nota (na aba e na lista) é a primeira linha não vazia.

## Onde ficam as notas

`~/.local/share/fastnotes/notes/*.md`. Nota apagada ou esvaziada vai para
`~/.local/share/fastnotes/trash/`. Mude o diretório com `FASTNOTES_DIR=/caminho`.
Os arquivos são Markdown comum: abrem em qualquer editor, Obsidian, Notion etc.

## Como funciona (referências)

- A renderização "ao vivo" segue a ideia do live preview do Obsidian/CodeMirror:
  os marcadores são substituídos por decorações e só reaparecem na linha do
  cursor ([atomic-editor](https://github.com/kenforthewin/atomic-editor) faz o
  mesmo em CodeMirror 6).
- A formatação de tabelas segue os formatadores clássicos (Prettier, extensões
  do VS Code): largura da coluna = célula mais larga, medindo a largura visível
  e respeitando `:---:`.
- Análise de Markdown própria, linha a linha (uma passada, sem alocações
  grandes), porque parsers completos como
  [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) dão o
  intervalo do bloco inteiro e não dos marcadores.

## Sistema de espaçamento e tipografia

Todos os tamanhos vêm de um conjunto pequeno de constantes no início de
`src/main.rs` (`SP1…SP8`, `HEADING_SCALE`, `MEASURE_EM`, `HEADER_H`…), para
que a interface tenha um ritmo único. As regras e de onde vêm:

| Regra | Valor | Base |
|---|---|---|
| Grade de espaçamento | 4 px; espaços preferem múltiplos de 8 (8, 16, 24, 32) | Convenção dos design systems (Material, Carbon, Fluent, Polaris): múltiplos de 8 dividem exatamente as densidades 1×, 1.5×, 2× e 3× de tela, e a escala discreta força decisões consistentes ([Spec: 8-pt grid](https://spec.fm/specifics/8-pt-grid), [designsystems.com](https://www.designsystems.com/space-grids-and-layouts/)). A proximidade agrupa: elementos próximos são lidos como um grupo (lei da proximidade da Gestalt). |
| Escala tipográfica | 1.25 (terça maior): corpo 1, H3 1.25, H2 1.5625, H1 2.0 | Escalas modulares derivadas de intervalos musicais, por Bringhurst (*The Elements of Typographic Style*); a terça maior dá incrementos moderados, adequados a interfaces ([Spec: type scale](https://spec.fm/specifics/type-scale), [UX-Republic](https://www.ux-republic.com/en/practical-guide-to-creating-a-modular-scale-type-for-your-interfaces/)). |
| Tamanho do corpo | 16 px por padrão (`Ctrl+=`/`Ctrl+-` ajustam, 10–40) | A leitura fluente cobre uma faixa ampla de tamanhos; a legibilidade e a compreensão na tela melhoram até ~18 pt ([Legge & Bigelow 2011](https://pubmed.ncbi.nlm.nih.gov/21828237/); [Rello et al. 2016](https://www.researchgate.net/publication/301935601_Make_It_Big_The_Effect_of_Font_Size_and_Line_Spacing_on_Online_Readability)). |
| Entrelinha | corpo 1.5; títulos 1.25; rótulos 1.35 | 1.5× dá leitura mais precisa e rápida que 1× na tela ([Ling & van Schaik 2007](https://www.sciencedirect.com/science/article/abs/pii/S0141938207000133)); Rello et al. recomendam não passar muito disso. Títulos curtos e grandes pedem entrelinha menor. |
| Medida (largura da coluna) | máx. 38 em ≈ 70 caracteres; coluna alinhada à esquerda (tabelas podem ir até a margem direita) | 55 caracteres por linha deram a melhor compreensão na tela ([Dyson & Haselgrove 2001](https://www.sciencedirect.com/science/article/abs/pii/S1071581901904586)); Bringhurst recomenda 45–75, Butterick 45–90 ([Practical Typography](https://practicaltypography.com/line-length.html)). |
| Tracking | fórmula do autor da Inter: `-0,0223 + 0,185·e^(−0,1745·tamanho)` em | [Inter dynamic metrics](https://d.rsms.me/inter-website/v3/dynmetrics/): ligeiramente mais aberto em tamanhos pequenos, mais fechado em títulos. |
| Alvos de clique | botões 32 px, linhas de lista 48 px, abas 32 px de altura, fechar aba 20 px com folga | WCAG 2.2 exige ≥ 24×24 px ([SC 2.5.8](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html)); Apple e Material usam 44 pt / 48 dp para toque; Parhi, Karlson & Bederson mediram ~9 mm para o polegar ([2006](https://www.microsoft.com/en-us/research/wp-content/uploads/2006/01/parhi-mobileHCI06.pdf)). |
| Cantos | 6 / 8 / 12 px (item, botão ou aba, painel) | Raio cresce com o tamanho do componente (Material 3: pequeno 8, médio 12). |
| Rótulos da interface | 13 px; secundários 12 px; teclas 11 px | A Inter foi desenhada para 14 px na tela; 12–13 px mantêm legibilidade nos rótulos. |
| Tabelas | números à direita, texto à esquerda, cabeçalho semibold seguindo a coluna; só réguas horizontais; célula 12×6 px; algarismos tabulares | Few, *Show Me the Numbers* (números à direita, mesma fonte legível); [Ström, Design better data tables](https://mattstromawn.com/writing/tables/) (algarismos tabulares reforçam a grade, cabeçalho curto); [NN/g](https://www.nngroup.com/articles/data-tables/) e [Eleken](https://www.eleken.co/blog-posts/table-design-ux) (cabeçalho alinhado ao dado, distinto); Tufte, razão dado-tinta (sem grade vertical, réguas suaves); [UX Movement, Data Table UX](https://www.youtube.com/watch?v=IPH8R17w3vA) (borda suave, mais respiro, números alinhados). |
| Quebra curta | Shift+Enter = `\` no fim da linha: a linha perde 0.45 em e seus glifos descem 0.225 em no desenho, então só a distância até a próxima linha diminui (1.5 → 1.05, 30 % menos) | Lei da proximidade (Gestalt): elementos próximos são lidos como grupo; a folga de 0.5 em vira 0.25 em dentro do grupo. |

Margens: 32 px nas laterais do texto, 24 px acima; cabeçalho 48 px e rodapé
32 px; painel de notas 384 px de largura com busca de 40 px.

## Publicar uma versão (Flathub)

O pacote da loja é o Flatpak `io.github.tevoetals.fastnotes`: manifesto e
fontes offline do cargo em `flatpak/`, metadados em `data/` (desktop,
AppStream, ícone, captura). O Flathub compila a partir da tag no GitHub, sem
rede, por isso `flatpak/cargo-sources.json` precisa acompanhar o `Cargo.lock`.

```sh
./release.sh 0.3.0 "O que mudou"   # versão, changelog, tag, push e Flathub
```

O script atualiza `Cargo.toml` e o `metainfo.xml`, regenera as fontes do
cargo, faz commit e tag `vX.Y.Z`, envia ao GitHub e copia o manifesto para o
clone do Flathub (`~/Programs/flathub-io.github.tevoetals.fastnotes`). O
Flathub compila e publica sozinho; a atualização aparece no Discover em cerca
de uma hora. Para testar o Flatpak localmente:

```sh
flatpak run org.flatpak.Builder --user --install --force-clean flatpak/build flatpak/io.github.tevoetals.fastnotes.yml
flatpak run io.github.tevoetals.fastnotes
```

## Depuração

- `FASTNOTES_TRACE=1` imprime o tempo de cada fase do startup e do primeiro quadro.
- `FASTNOTES_SNAPSHOT=quadro.ppm` grava o último quadro renderizado.
- `FASTNOTES_TEST_FIFO=/caminho/fifo` injeta eventos sintéticos (usado nos testes).
- `FASTNOTES_PROFILE=1` imprime, a cada 30 quadros, o tempo médio por fase
  (cabeçalho, shaping, glifos, rodapé, quadro inteiro, entrada de teclado…).

## Consumo de recursos

- **Memória:** a janela usa dois quadros em memória compartilhada (`wl_shm`),
  2,5 MB cada numa janela de 956×666; um terceiro só se o compositor segurar
  os dois. O pool é recriado a cada mudança de tamanho, então nunca cresce.
  As fontes curadas (Noto) ficam mapeadas do disco; o banco completo de fontes
  do sistema só é carregado se uma nota tiver caracteres fora delas (CJK,
  árabe, hebraico…) e, nesse caso, a memória compartilhada de arquivos sobe.
- **CPU:** zero parado (sem timers). Cada tecla custa ~1 ms de layout e ~3 ms
  de desenho numa janela de 1200×800; o cursor pisca por 5 s depois da última
  tecla e depois para. O buffer é XRGB opaco, então o compositor não precisa
  misturar nada.

## Limitações

- Só Wayland (sem X11) e sem IME (`text-input-v3`) por enquanto: métodos de
  entrada como fcitx/ibus não são usados; dead keys e AltGr funcionam.
- Escala de tela inteira (1×, 2×); escala fracionária ainda não.
- Células de tabela são de uma linha (Markdown não tem quebra dentro da célula).
- Toggles e colunas usam sintaxe própria (`▾`, `:::`, `|||`): em outros editores aparecem como texto simples.

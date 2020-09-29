# pepper
Experimental code editor

# development thread
https://twitter.com/ahvamolessa/status/1276978064166182913

# usando
- f/t/F/T tem que levar em conta que insercoes sao feitas *antes* dos cursores
- `N` e `P` tem que levar em conta a palavra e nao apenas se ta na mesma posicao (pode ter uma busca que eh so comeco da palavra me questao)
- scrolling nao ta certo quando tem linhas longas acima do cursor
- fazer o cursor pular pra busca mais proxima enquanto busca (??)

# todo
- ~~undo/redo~~
	- ~~store/apply edit diffs~~
	- ~~compress history when adding edits~~
	- ~~fix cursor position on multiple cursors~~
- ~~modes~~
	- ~~basic implementation~~
	- ~~key chords actions~~
- ~~selection~~
	- ~~swap position and anchor~~
	- ~~selection merging~~
- ~~multiple cursors~~
	- ~~merge cursors~~
- ~~long lines~~
- ~~search~~
	- ~~highlight search matches~~
	- ~~navigate between search matches~~
- ~~operations~~
	- ~~delete~~
	- ~~copy~~
	- ~~paste~~
- ~~client/server model~~
	- ~~dumb client sends events and receives display bytes~~
	- ~~track client that last sent event (focused)~~
	- ~~show status messages on focused client~~
- ~~custom bindings~~
	- ~~custom bindings expand to builtin bindings~~
	- ~~custom bindings take precedence~~
	- ~~define custom bindings in config file~~
- ~~script (command) mode~~
	- ~~execute script line and preserve context~~
	- ~~builtin bindings~~
- ~~syntax highlighting~~
	- ~~simple pattern matching~~
	- ~~define language syntaxes~~
	- ~~calculate highlight ranges when code changes~~
	- ~~recalculate only changed portions of buffer~~
	- ~~show whitespace with correct colors~~
- ~~utf8~~
- ~~file operations~~
	- ~~edit (command to open/create file?)~~
	- ~~save~~
	- ~~reuse buffer if already open~~
	- ~~remove all buffer views when closing a buffer~~
- ~~external commands~~
	- ~~spawn commands (processes) that execute on the server~~
- ~~cli~~
	- ~~custom session name~~
	- ~~config path~~
	- ~~send keys to server~~
	- ~~open files~~
- ~~autocomplete~~
	- ~~select/entries ui~~
	- ~~selection movements~~
	- ~~clear/change entries~~
	- ~~completion suggestion as typing word~~
	- ~~accept completion~~
	- ~~word database~~
- ~~text objects~~
	- ~~inner word~~
	- ~~outer word~~
	- ~~inner balanced braces~~
	- ~~outer balanced braces~~
- status bar
	- ~~buffer name~~
	- ~~buffer position~~
	- buffered keys
- scripting
	- ~~integrate lua to use as command interface~~
	- ~~config file is lua script~~
	- ~~builtin bindings~~
	- full api exposed
- code navigation
	- ~~go to line (new mode)~~
	- ~~word forward/backward~~
	- ~~home/end/first-column~~
	- ~~half-page down/up~~
	- ~~find char~~
	- ~~center main cursor, main cursor at top, main cursor at bottom (`zz`, `zk`, `zj`)~~
	- ~~go to matching bracket (`gm`)~~
	- ~~select all text in buffer~~
	- ~~limit selections to search ranges (or cursor = search ranges if empty) (`?`)~~
	- ~~cursor history navigation~~
	- remember column when moving between lines
- selections
	- ~~select current word and search~~
	- ~~add cursor on 'next search result' (`*`)~~
	- ~~skip one and add cursor on 'next next search result'~~
	- ~~remove selections (set anchor to position)~~
	- ~~swap anchor and position~~
	- ~~select cursor lines~~
	- ~~keep only main cursor~~
	- ~~add cursor to each selection line~~
	- cursor movement kind AnchorThenPosition (??)
- editing
	- ~~edit new line bellow/above (`o` and `O`)~~
	- ~~keep current identation~~
	- delete identation
- macros (??)
	- repeat last insert (`.`)
	- record/play custom macros
- language server protocol
- debug adapter protocol

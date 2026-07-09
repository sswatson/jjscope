## Configuring keybindings

```toml
# change keybinding
save = "ctrl+s"
# set multiple keybindings
save = ["ctrl+s", "ctrl+shift+g"]
# disable keybinding
save = false
```

In below examples default values are used.

### Top-level scroll bindings

These apply as defaults to all scroll-capable components and can be overridden
in each component's own section.

```toml
[jjscope.keybinds]
scroll-down = ["j", "down"]
scroll-up = ["k", "up"]
scroll-down-half = "shift+j"
scroll-up-half = "shift+k"
```

### Message popup

Overrides top-level scroll bindings. `scroll-down-page` and `scroll-up-page`
are only configurable here.

```toml
[jjscope.keybinds.message-popup]
scroll-down = ["j", "down"]
scroll-up = ["k", "up"]
scroll-down-half = "ctrl+d"
scroll-up-half = "ctrl+u"
scroll-down-page = ["ctrl+f", "space", "pagedown"]
scroll-up-page = ["ctrl+b", "pageup"]
```

### Log tab

```toml
[jjscope.keybinds.log-tab]
save = "ctrl+s"
cancel = "esc"

close-popup = "q"

scroll-down = ["j", "down"]
scroll-up = ["k", "up"]
scroll-down-half = "shift+j"
scroll-up-half = "shift+k"

focus-current = "@"
toggle-diff-format = "w"

refresh = ["shift+r", "f5"]
create-new = "n"
create-new-describe = "shift+n"
insert-new = "i"
insert-move = "shift+i"
duplicate = "shift+d"
rebase = "ctrl+r"
squash = "s"
squash-ignore-immutable = "shift+s"
edit-change = "e"
edit-change-ignore-immutable = "shift+e"
abandon = "a"
absorb = "shift+a"
metaedit-update-change-id = "c"
metaedit-update-change-id-ignore-immutable = "shift+c"
describe = "d"
edit-revset = "r"
set-bookmark = "b"
open-files = "enter"
copy-change-id = "y"
copy-rev = "shift+y"

push = "p"
push-all = "shift+p"
fetch = "f"
fetch-all = "shift+f"

open-help = "?"
```

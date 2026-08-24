# optination

A cursor theme picker for Hyprland that shows you the cursors.

Choosing a cursor theme from a numbered list is guesswork — `Quintom_Ink` and
`Quintom_Snow` are just two words until you can see them. optination lists every
theme on the system with its actual pointer, I-beam, hand, resize, busy and
no-drop shapes rendered at the size you are about to use, applies your pick to
the live pointer the moment you click it, and only writes to disk when you say so.

Built with [Slint](https://slint.dev) and the
[material component library](https://github.com/slint-ui/material-rust-template),
vendored under `material-1.0/`, to a Material 3 design handoff.

## Layout

A title bar over two panes. The window keeps its normal frame: Hyprland already
draws the rounding and the shadow, and painting our own left a
transparent-cornered surface the tiler had to work around.

- **Title bar** — app mark, `Cursor / Appearance · Pointer`, then the accent
  menu and the light/dark toggle in the top-right corner.
- **Left pane** — search field, filter chips, and one row per theme: the theme's
  own pointer rasterized into a 44px tile, its name, and a meta line of format,
  shape count, tone and handedness.
- **Right pane** — the live preview (pointer at 1.9× the chosen size, the other
  five shapes at 1.25×, on the accent's lightest tone under a dot grid), the size
  slider with tick buttons at 16/24/32/48/64/96, the resolved `hyprctl setcursor`
  line, and Reset / Apply.

There is one light/dark system, the app's own. The preview surface follows it
along with everything else.

Only the selected theme is rendered at full size, and the row thumbnails are
rendered once at a fixed size, so moving the slider re-renders one theme rather
than all sixty-nine.

## Accents

Six M3 tonal accents — Cyan, Purple, Green, Amber, Rose, Indigo — each with a
light and a dark scheme. Picking one rebuilds the whole `MaterialPalette` scheme,
not just the parts this app paints, so the vendored slider, switch and buttons
re-tone with it.

**Follow Omarchy theme** reads `accent` and `mode` from
`~/.local/state/omarchy/current/theme/colors.toml` and snaps to the nearest of
the six. It is a nearest-match, not a generated palette: Omarchy gives one accent
hex, and deriving a full M3 tonal scheme from it would need a tonal-palette
generator this app does not ship.

## Why

It replaces a shell function that had two problems beyond being blind:

- **It could only see half the themes.** It globbed for `*/cursors/`, which is
  the Xcursor layout. Every hyprcursor theme — `manifest.hl` plus
  `hyprcursors/*.hlc` — was invisible to it. On the machine it was written for
  that was 16 of 69 themes.
- **Its "make permanent" wrote to a file nothing reads.** It appended
  `env = XCURSOR_THEME,…` to `~/.config/hypr/hyprland.conf`, which Omarchy 4
  stopped reading when the Hyprland config moved to Lua. The setting looked
  saved and silently was not.

## What it handles

| Format | Layout | Assets |
|---|---|---|
| Xcursor | `<theme>/cursors/<shape>` | binary, pre-rasterized ARGB at fixed sizes |
| hyprcursor | `<theme>/manifest.{hl,toml}` + `<theme>/hyprcursors/<shape>.hlc` | zip of `meta.hl` + SVG **or** PNG |

Both manifest dialects and both hyprcursor asset kinds are real and in the wild;
a picker that only handles `manifest.hl` or only handles SVG will quietly drop
themes.

## Filtering

A search box over name, comment, format and tone, plus chips:

| Group | Chips | Derived from |
|---|---|---|
| Format | `X11`, `hyprcursor` | which directories the theme ships |
| Tone | `light`, `dark` | a `light`/`white`/`dark` word in the theme name |
| Handedness | `standard`, `mirrored` | a `right`/`lefthand` word in the theme name |
| Source | `system`, `user` | `/usr/share/icons` vs. under `$HOME` |

Chips within a group are OR-ed and the groups are AND-ed, so `hyprcursor` + `dark`
narrows to dark hyprcursor themes instead of producing the empty set. A group with
nothing checked places no constraint.

Tone and handedness are read off the theme *name*, which is the only place the
information is recorded — a theme that declares neither stays untagged rather than
being guessed at from its pixels.

## Missing shapes

Not every theme defines every shape. Where one is absent the preview cell shows a
dash rather than an unexplained hole, and `--check` lists it:

```
$ optination --check 32
rose-pine-hyprcursor                     missing: busy
1 theme(s) with an undrawable preview slot at 32px
```

That is a gap in the *theme* — Hyprland will fall back to another theme's shape at
runtime — and this is how you find out before committing to it. A shape that
decodes but rasterizes to fully transparent pixels counts as missing too, since on
screen the two are the same thing.

## Applying

Three consumers need telling, and none of them covers the others:

| Mechanism | Reaches | Lifetime |
|---|---|---|
| `hyprctl setcursor` | Hyprland's own pointer | immediate, until reload |
| `gsettings set org.gnome.desktop.interface cursor-theme` | GTK apps, settings portal | the session |
| `hl.env("XCURSOR_THEME", …)` in `~/.config/hypr/looknfeel.lua` | every client's environment | permanent |

Clicking a theme does the first two, so the pointer under your hand changes while
you browse. **Apply** adds the third, writing a marked block:

```lua
-- >>> optination (managed block) >>>
hl.env("XCURSOR_THEME", "Bibata-Modern-Ice")
hl.env("XCURSOR_SIZE", "30")
hl.env("HYPRCURSOR_THEME", "Bibata-Modern-Ice")
hl.env("HYPRCURSOR_SIZE", "30")
-- <<< optination (managed block) <<<
```

Rewritten in place on each save, never duplicated, with a timestamped backup of
the file alongside it. `looknfeel.lua` is a user file that Omarchy loads after
its own defaults, so the `XCURSOR_SIZE` in `default/hypr/envs.lua` is overridden
rather than fought with. After writing, `hyprctl reload` and
`hyprctl configerrors` run, and a config error is reported instead of waiting to
bite at next login.

**Reset** puts back whatever was set when the app opened, so trying things on live
is free.

## CLI

The thing this replaced was a shell function, and shell functions get called
from scripts.

```
optination                        open the picker
optination --list                 every theme found: name, format, shape count
optination --check [SIZE]         report which preview shapes a theme cannot draw
optination --apply <THEME> [SIZE] apply for this session
optination --save  <THEME> [SIZE] apply and persist
optination --current              print the active theme and size
```

`--check` reports gaps in the *themes*, not in optination — a theme with no
`wait`/`watch` shape will fall back to another theme's at runtime, and this is
how you find that out before you commit to it.

## Build

CI builds every push and uploads a Linux x86-64 binary as the
`optination-linux-x86_64` artifact. To build locally instead:

```sh
cargo build --release
install -Dm755 target/release/optination ~/.local/bin/optination
```

No system libraries beyond Wayland and a GPU driver. Xcursor parsing via
`xcursor`, SVG via `resvg`, PNG via `png`, hyprcursor archives via `zip`.

## Notes

- Cursors are drawn at their true pixel size — the preview is the thing itself,
  not an approximation of it.
- For themes shipping both formats, the hyprcursor version is previewed, since
  that is the one Hyprland will actually use.

## License

MIT. The vendored `material-1.0/` directory is MIT, copyright SixtyFPS GmbH — see
`material-1.0/LICENSE.md`.

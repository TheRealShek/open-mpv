# Configuration

open-mpv works without a configuration file. Create one only when you want a
different default every time the app starts. You do not need to copy every
setting: add only the lines for the behavior you want to change.

Use the in-app controls when a change is temporary, such as zooming one image,
changing one video's volume or selecting a subtitle track. Use this file when
you want open-mpv to make the same choice again on its next launch.

Settings are loaded when the open-mpv process starts. After editing the file,
quit every open-mpv window and open the app again.

## Create the file

The optional configuration file is:

```text
~/.config/open-mpv/open-mpv.conf
```

Create its directory if it does not exist:

```sh
mkdir -p ~/.config/open-mpv
```

The format uses one `key = value` assignment per line. Spaces around `=` are
optional. Empty lines are ignored. Comments begin with `#` and must be on their
own line. A `#` inside a value, such as a background color, remains part of
that value. For settings with a yes-or-no choice, use `yes` or `no`.

For example, this configuration is for someone who prefers newest files first,
opens media at actual size, starts videos quietly without subtitles, and does
not want `Q` to quit:

```ini
# A CSS color for the empty canvas around the media.
background = #202020

# Show the most recently modified file first.
sort = date

# Start newly opened media at 100% instead of fitting it to the window.
fit = actual

# Start videos at 70% volume with subtitles disabled.
volume = 70
subtitles = off

# Remove the default Q shortcut without assigning another action.
bind = q none
```

Unknown settings, invalid values, keys that GTK cannot read and unknown action
names produce a warning but do not prevent open-mpv from starting. See the
[README troubleshooting section](../README.md#troubleshooting) to find those
messages.

## Choose what to change

### Canvas and opening view

| Setting | Default | Use it when | Values and effect |
| --- | --- | --- | --- |
| `background` | `#121212` | You want a different canvas color around the media. | A GTK CSS color such as `black`, `#202020` or `rgb(20, 20, 20)`. |
| `fit` | `fit` | You always want media to open fitted or at its original pixel size. | `fit` shrinks large images to the window without enlarging small images; video may scale up or down. `actual` maps one media pixel to one physical screen pixel. You can still switch modes while viewing. |
| `start-fullscreen` | `no` | You normally view media without window chrome. | `yes` starts the first window fullscreen; `no` starts in a regular window. |

### Folder order and navigation

These settings affect the shared list of supported images and videos in the
current folder.

| Setting | Default | Use it when | Values and effect |
| --- | --- | --- | --- |
| `sort` | `name` | You prefer browsing by filename or by recent changes. | `name` uses case-insensitive natural order, so `image2` comes before `image10`. `date` uses modification time with the newest file first. |
| `sort-reverse` | `no` | You want the opposite of the selected order. | `yes` reverses it: name order becomes descending, or date order becomes oldest-first. |
| `wrap` | `no` | You want Next at the last file to continue from the first file, and Previous to do the reverse. | `yes` enables this cycle; `no` stops at either end. |

### Controls and pointer

| Setting | Default | Use it when | Values and effect |
| --- | --- | --- | --- |
| `overlay-timeout` | `2.0` | Controls disappear too quickly or stay visible too long. | Seconds after the last pointer activity. Use `0.2` or more; smaller non-negative values are treated as `0.2`. |
| `hide-cursor` | `yes` | You want the pointer to remain visible after the controls fade. | `yes` hides it with the controls; `no` leaves it visible. Moving the pointer shows the controls again. |

### Video defaults

These are defaults for video playback. Playback controls can still change
volume, mute state and subtitles during the current open-mpv session.

| Setting | Default | Use it when | Values and effect |
| --- | --- | --- | --- |
| `loop` | `yes` | You do or do not want videos to restart at the end. | `yes` restarts video playback; `no` leaves the final frame visible. Animated images always loop. |
| `volume` | `100` | Videos consistently start too loud or too quiet. | A percentage from `0` to `150`. Values above `100` amplify beyond the normal level and may distort. |
| `subtitles` | `auto` | You want videos to start without subtitles. | `auto` lets GStreamer select a subtitle track; `off` starts with text tracks disabled. |

### Neighbor image cache

| Setting | Default | Use it when | Values and effect |
| --- | --- | --- | --- |
| `cache-budget-mb` | `256` | You want faster neighboring-image navigation or lower memory use. | Maximum extra decoded-image memory in MiB beyond the image currently displayed. `0` keeps no decoded neighbors. Videos are never stored in this cache. |

The displayed image can itself use more memory than this budget. Lowering the
budget saves memory at the cost of decoding nearby images again when you reach
them.

## Custom key bindings

A binding has a GTK key name followed by an open-mpv action:

```text
bind = <key> <action>
```

Add one `bind` line for each key you want to change. A configured key replaces
the default action for that key; it does not add a second action. Multiple keys
may point to the same action. Use the special action `none` to remove a default
binding.

Common key forms include:

- Letters: `n`, `q`
- Named keys: `Delete`, `Page_Down`, `bracketright`
- Modifiers: `<Shift>d`, `<Control>o`, `<Control><Shift>o`

Press `?` in open-mpv to see the active shortcuts after configuration is
applied.

### Available actions

| Category | Action names |
| --- | --- |
| Open and navigate | `open-file`, `open-folder`, `next`, `prev`, `first`, `last` |
| Contextual arrows | `right`, `left`, `up`, `down` |
| Playback | `play-pause`, `seek-back`, `seek-forward`, `speed-down`, `speed-up`, `speed-reset`, `mute`, `volume-up`, `volume-down` |
| Subtitles | `subtitle-open`, `subtitle-toggle`, `subtitle-cycle` |
| View | `zoom-in`, `zoom-out`, `zoom-fit`, `zoom-actual`, `zoom-toggle`, `rotate-cw`, `rotate-ccw`, `fullscreen` |
| Quick Markup | `markup`, `markup-box`, `markup-arrow`, `markup-copy`, `markup-clear` |
| File and session | `save`, `trash`, `undo`, `help`, `close`, `escape` |

The contextual arrow actions preserve open-mpv's normal behavior: horizontal
arrows navigate unless the media is zoomed and can be panned, while vertical
arrows adjust video volume unless they can pan the media. Use `next`, `prev`,
`volume-up` or `volume-down` when you want only the named behavior.

Other actions whose names may not be self-explanatory:

- `play-pause` pauses or resumes video; on a still image it opens the next file.
- `subtitle-open` lets you attach a local subtitle file to the current video.
- `save` writes a supported image rotation back to the source file.
- `undo` undoes the latest Quick Markup shape or the latest offered trash
  action, depending on the current context.
- `close` quits immediately. `escape` first cancels the active mode or leaves
  fullscreen, then closes only when there is nothing else to dismiss.

## Return to defaults

Remove a setting's line to restore only that setting's default. Remove the
whole `open-mpv.conf` file to restore every default. Restart open-mpv after
either change.

# open-mpv Context

open-mpv helps people view local visual media quickly and safely without
turning their files into a managed library or relying on an online service.

## Language

**Local media**:
Images and videos available from the user's device or accessible storage,
rather than media supplied by an online service.

**Local-media viewer**:
A public application for viewing images and videos as equally central media
types. It is narrower than a general media player and is not primarily an
image viewer with incidental video support.
_Avoid_: Image viewer, general media player

**User**:
Anyone who uses open-mpv to view local media. The author's workflow may inform
the product, but the author is not its only intended user.
_Avoid_: Author-only user, personal tool

**Officially supported environment**:
An environment in which open-mpv regularly completes its required automated
checks and human desktop testing. Other environments may work, but are not
represented as supported without that evidence.

**Product capability**:
A behavior that directly improves opening, viewing, navigating, or safely
handling local media. A managed library, persistent catalog, online service,
or general editing workflow is outside the product boundary.
_Avoid_: Feature that is unrelated to the local viewing workflow

**Foundation hardening**:
Work that makes the local-media viewer's existing promises demonstrably true
before its product boundary expands.

**Explorer**:
A visual browsing surface for the media in one selected folder. Opening a
folder enters the Explorer; choosing an item opens it in the Viewer, and a
different folder is selected through the open-folder flow rather than by
turning the Explorer into a filesystem browser.
_Avoid_: Library, media catalog

**Viewer**:
The surface for viewing one chosen local-media item. Opening a media file
enters the Viewer directly.

**Navigation set**:
The supported images and videos in the selected folder, presented in one
shared order. A Filename filter temporarily narrows this set when an item is
opened from filtered Explorer results; clearing the filter restores the full
folder set. Media type never splits navigation into separate image and video
sequences.
_Avoid_: Playlist, image-only sequence, video-only sequence

**Preview**:
A static visual representation of a media item in the Explorer. An image uses
an image thumbnail; a video uses a representative frame when video previews
are enabled.
_Avoid_: Animated preview, autoplay preview

**Media-type badge**:
A small corner icon on an Explorer item that identifies it as an image or
video independently of whether a Preview is available.
_Avoid_: Preview

**Progressive preview loading**:
The Explorer knows the complete sorted Navigation set but materializes
Previews only for visible and nearby items as the user scrolls. It is a
continuous browsing experience rather than a sequence of explicit pages.
_Avoid_: Pagination, loading every preview

**Selected item**:
The one Explorer item that receives contextual actions such as Open or Trash.
Opening a folder initially selects and keyboard-focuses its first item without
opening the Viewer. One click selects an item; double-click or Enter opens it.
Explorer does not provide multi-selection or batch file operations.
_Avoid_: Selection set, batch selection

**Filename filter**:
A transient way to narrow the Explorer items shown from the selected folder by
filename. It creates no index, saved search, or persistent catalog.
_Avoid_: Search index, saved search, library search

**Explorer session**:
The selected folder together with its transient Filename filter, Selected
item, and scroll position. Opening an item keeps this session as the Back
destination; Back or Escape from that Viewer returns to the same state.

**Info panel**:
A deliberately opened, read-only surface for relevant facts about the current
image or video. It remains absent from normal Viewer and Explorer chrome and
does not delay opening media to gather expensive information.
_Avoid_: Permanent metadata sidebar, raw metadata dump

**External handoff**:
A contextual action that reveals the current media item in the desktop file
manager or asks another installed application to open it. Handoff belongs in a
secondary menu rather than permanent Viewer controls.

**Progressive disclosure**:
The default viewing experience exposes only the controls needed for ordinary
viewing. Advanced capabilities appear only when the current media or a
deliberately entered mode makes them relevant.
_Avoid_: Permanent advanced toolbar, always-visible editing controls

**Edit mode**:
A future, deliberately entered workspace for changing images. Editing may grow
over time, but its controls do not compete with the simple default Viewer.
Video editing is not part of the current product direction and requires a
separate future planning decision before entering scope.
_Avoid_: Default editing surface, current video-editing capability

**Editing session**:
The transient, reversible changes made while Edit mode is active. An Editing
session never changes source media automatically; only an explicit save action
commits its result.
_Avoid_: Autosave, implicit source modification

**Save a Copy**:
The primary way to commit an Editing session, creating a new media file while
leaving the source unchanged. Replacing the source is a separate, explicitly
chosen action.

**Quick Markup**:
A transient box-and-arrow workflow whose result is copied to the clipboard.
It does not modify the source media or become a persistent editing session.
_Avoid_: Edit mode, persistent annotation

**Copy**:
A contextual clipboard action that copies a static image, an SVG at its
intrinsic dimensions, the current animated-image frame, or the Quick Markup
result. It does not create or modify a file, and video-frame capture is outside
the current direction.

**Audio-track selection**:
A contextual choice offered only when a video contains multiple audio tracks.
It belongs in a secondary menu rather than the normal transport controls.
_Avoid_: Permanent audio-track control

**Focused playback**:
The contextual controls needed to view animated images and local videos,
including pause and resume for either medium. Specialized frame analysis and
automatic slideshow navigation are outside the current product direction.
_Avoid_: Frame stepping, slideshow mode, general media-player controls

**Preferences**:
A graphical wrapper over the canonical human-editable configuration file for
common settings, applying changes immediately through atomic updates. It does
not create a second settings store and preserves manual comments, advanced
options, keybindings, and unrecognized content.
_Avoid_: Separate GUI settings, destructive config rewrite

**Native package**:
A distribution package built for and integrated with a supported operating
system rather than delivered through a cross-distribution application bundle.
Native packages are the preferred way to ship open-mpv; Flatpak is reserved
for a need that justifies its additional platform boundary.

**Reference environment**:
Fedora Workstation on GNOME and Wayland, where open-mpv first earns official
support through complete verification. Other Linux and Wayland environments
may become supported with native packaging and equivalent evidence; X11 and
non-Linux platforms are not current product commitments.

**Workspace**:
The one active application window containing Explorer, Viewer, and Edit mode.
Multiple windows require a separate future product decision motivated by a
concrete workflow such as side-by-side comparison.
_Avoid_: Window per media item, current multi-window workflow

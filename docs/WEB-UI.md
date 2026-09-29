# Operator workspace

Cellar embeds its interface in the binary. It needs no frontend service, CDN,
or internet connection to render. The mug and Azure/Frost palette follow
AppleJack's branding, with light, dark, and system themes.

## Navigation and scope

Choose a server in the instance row, then choose a section in the desktop rail
or the phone's **Section** selector. The heading always names the selected
server and document scope. Switching servers clears the command draft and
console, cancels old reads, and reconnects the event stream for the new server.
Delayed replies cannot become the new server's data.

Use **Jump to** or Ctrl+K (Cmd+K on macOS) to find a section, server, or gamemode
command. Arrow keys move through results, Enter selects, and Escape cancels.
The desktop rail and nested tabs support arrow keys, Home, and End. A skip link
goes directly to the workspace.

| Section | Operator job |
| --- | --- |
| Overview | Health, players, resources and recent output in a personal layout |
| Console | Live output, command history, filters and gamemode commands |
| Monitoring | Resource history, addresses and anti-cheat evidence |
| Players | Connected roster, recorded history and invite gate |
| Database | Shared database inspection and optional direct control |
| Diagnostics | Preflight, runtime checks, scheduled jobs and parser evidence |
| Activity | Commands and lifecycle events in the selected scope |
| Records | Bridge documents, revisions and deletion |
| Configuration | Profiles, live convars, import/export and build tools |
| Settings & recovery | Authentication, backups, snapshots and process controls |

**Full status** expands every status value on any screen size. Values wrap
rather than disappear behind an ellipsis. Lifecycle advice distinguishes a
process that is starting, unhealthy, crash-looping or has failed to stop.
Connection loss is separate from game health. Stale status disables lifecycle
and command controls until Cellar answers again.

## Overview

**Edit layout** chooses modules and their order. Drag cards or use the labelled
earlier/later controls. Focus a resize handle and use Left/Right, Home or End to
change its span with a keyboard. Wider health, player and resource cards expose
more detail. Phone cards use the available width. The layout is saved in this
browser, and Reset default restores the initial arrangement.

Live updates preserve card controls and focus. Collapsible panels remember
their open state separately from the overview layout.

## Console

Filter output by text, view, minimum level or category. **Filters** reveals the
less frequent controls. Search full log queries persistent logs rather than
only the browser's recent buffer. Pause stops painting while retaining incoming
records. Auto-scroll is an explicit toggle. Timestamps and categories remain
visible on phones, with messages wrapping below them.

Type a command and press Enter or **Send**. Progress distinguishes sending,
reply received, refusal and an unknown outcome after a connection failure.
Up/Down recall commands from this page's memory; commands are not persisted to
browser storage because they can contain credentials. Tab completes from the
active profile's command catalogue. Commands execute at full engine privilege.

The browser retains at most 5,000 records and displays at most 1,500 rows.
Rendering and scrolling are batched per animation frame. Reconnects recover
available log lines and state how many lines were recovered or events missed.

## Recovery and destructive actions

Settings has jump links for access, database backups, persistence and process
controls. Database restore applies to the shared database and stops all managed
servers. Persistence restore applies to the selected document scope. Both leave
servers stopped for inspection. The confirmation names the consequence and
preserves the required typed phrase or filename.

Shut down Cellar and Kill everything are process-wide. The latter skips graceful
engine shutdown and requires `KILL ALL`. A dropped connection cannot prove that
a destructive operation succeeded; inspect the host before retrying.

## Accessibility and local preferences

The interface supports 320px screens, 200% text, keyboard navigation, visible
focus, reduced motion and touch targets of at least 44px. Tables scroll within
labelled, focusable regions. Status uses words and distinct glyphs as well as
colour. Important operation and connection changes have live announcements.

Browser notifications are opt-in and depend on browser permission. They are
delivered while the page is connected, not through a background Web Push
subscription. The service worker does not cache operator pages or API data.

For remote access, use the [HTTPS deployment guide](REMOTE-ACCESS.md).

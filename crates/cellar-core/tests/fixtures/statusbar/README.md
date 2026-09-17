# Real s&box status-bar fixtures

`real-pty-2026-09-17-build-24826151.hex` is an exact hexadecimal encoding of one
status redraw captured from the PTY of the real Windows `sbox-server.exe`
(Steam app 590830, build 24826151) running under Wine 11.13 on 2026-09-17. The
terminal was 64 columns and the deliberately long hostname forced the engine's
two halves together. The capture contains the cursor sequences, two blank
chrome lines, both status lines, doubled CRLF bytes, and the next redraw's
cursor movement. It was captured with:

```sh
timeout --signal=TERM --kill-after=12s 90s script -q -e \
  -c 'stty cols 64 rows 24; exec env WINEDEBUG=-all wine ./sbox-server.exe +game facepunch.sandbox +hostname CellarIndependentFixtureLongName +port 28065 +queryport 28066' \
  /tmp/cellar-statusbar-wine.typescript
```

`real-status-lines-2026-09-17-build-24826151.txt` contains the two cleaned
status lines from that redraw. Neither current fixture was produced by Cellar's
renderer.

`real-pty-2026-08-24.hex` is the corresponding historical 80-column capture.
Its exact Steam build ID was not retained.

`real-status-lines-2026-08-24.txt` contains cleaned lines from that capture and
another real run that crossed the engine's rounded-hour boundary. The second
run had been live for about 38 minutes when the engine rendered hour `1`.

Update these fixtures only from a real dedicated-server PTY and record the
Steam app, build ID, date, terminal width, and capture command here.

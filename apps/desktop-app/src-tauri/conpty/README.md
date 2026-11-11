# Bundled ConPTY

`conpty.dll` and `OpenConsole.exe` (x64) are Microsoft's side-by-side
ConPTY, taken unmodified from the NuGet package
`Microsoft.Windows.Console.ConPTY` 1.25.260930003
(https://www.nuget.org/packages/Microsoft.Windows.Console.ConPTY/1.25.260930003),
MIT licensed, (c) Microsoft Corporation — see `LICENSE`.

Senju Term ships them next to `senju-term.exe` (Windows only) so that OSC 7 and
OSC 133 sequences survive the pseudo console; the inbox ConPTY drops them.

These files are generated. Do not edit or replace them by hand: change the
pinned version and hashes in `apps/desktop-app/scripts/fetch-conpty.mjs` and
re-run it. `node scripts/fetch-conpty.mjs --check` verifies them offline.

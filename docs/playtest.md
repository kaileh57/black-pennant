# Playtesting Black Pennant

Thanks for testing. This page covers what to download, how to open a build that is not signed, what to play, and how to send your logs back. A session takes about two hours: the tutorial, three campaign loops, then the sandbox and a skirmish.

Playtest builds are pre-releases on the [releases page](https://github.com/kaileh57/black-pennant/releases). They are early: expect rough edges, and expect saves to break between versions.

## What you need

These are the minimum machines the game is built for:

- **Mac:** an Apple M1 or later with 8 GB of memory, on macOS 14 Sonoma or later. Intel Macs cannot run the game.
- **Windows:** Windows 10 or 11, 64-bit, an Intel Core i5-8400 or AMD Ryzen 5 2600, 16 GB of memory, and a graphics card with 6 GB of video memory (RTX 2060 or RTX 3050 6 GB class) and a current Direct3D 12 or Vulkan driver, at 1080p.

Slower machines are welcome too. Say what you have in the report, because reports from below the minimum help as much as reports from above it.

## Download

From the newest pre-release, download one zip:

- `black-pennant-<version>-macos-arm64.zip` for a Mac.
- `black-pennant-<version>-windows-x86_64.zip` for Windows.

`SHA256SUMS.txt` lists each zip's checksum. With the GitHub command line installed, `gh attestation verify <zip> --repo kaileh57/black-pennant` checks that GitHub's own runners built the file from this repository.

## Open a build that is not signed

The project spends no money, so it buys no code-signing certificate from Apple or Microsoft. Both systems therefore treat the game as an app from an unknown developer and warn you the first time. The warning says nothing about the code itself: every build is made by GitHub's runners from the public source, and the attestation above proves which commit it came from.

### macOS

The app carries only a local ("ad-hoc") signature, with no Apple Developer ID and no notarization.

1. Unzip the file and move Black Pennant to Applications.
2. Open Black Pennant. macOS says it cannot verify the app and will not open it. Close that message.
3. Open System Settings, then Privacy & Security. Scroll down to Security, where Black Pennant is named, and choose Open Anyway.
4. Confirm with your password or Touch ID, then choose Open Anyway once more.

On macOS 14 you can instead Control-click (or right-click) the app, choose Open, then Open again. macOS 15 and later removed that shortcut, so use Privacy & Security there. Each new version asks once more.

### Windows

1. Unzip the whole folder somewhere you can find it, such as your Desktop. Keep the files together: the game needs `Black Pennant.pck` and `gdclient.dll` beside the executable.
2. Open `Black Pennant.exe`. SmartScreen shows "Windows protected your PC", because the file has no signature and no download history yet.
3. Choose More info, then Run anyway.

Smart App Control, which is on by default only on fresh installs of Windows 11, blocks unsigned programs and allows no exceptions. If it blocks the game, the game cannot run on that machine unless you turn Smart App Control off in Windows Security, under App & browser control.

## The first start

The game first asks whether to send crash reports. Either answer is fine. Until a release's notes say otherwise, playtest builds have no crash-report service set up, so nothing is sent whichever you choose. Your crash logs stay on your machine, and the next section says how to attach them.

The first start also runs a short graphics check to pick a quality level. Let it finish.

## What to play

Please play these in order, and take a short note of anything that confused you as you go.

1. **The tutorial.** From the main menu choose Start the tutorial. It takes about 10 minutes: widen a fast attack craft, fit a gun, radar, engine and fuel tank, fix a blocked gun arc, then test the ship in the basin and fight. Finish it if you can, and if you stop, say where.
2. **Three campaign loops.** Choose Start a campaign on Normal. One loop runs from opening a briefing to opening the next one: read the briefing, refit in the dry dock, optionally press Space for a splash test, sail the sortie, read the after-action report, then choose the next move on the chart table. Play at least three loops. Pause freely at sea: sortie pauses over 2 minutes are left out of the timing log. Time at the chart table, in a briefing or in the dry dock counts as play even when you are away, so mention any long break there in your report.
3. **The sandbox.** Choose Open the sandbox, build or change a ship, and fight with it.
4. **A skirmish.** Choose Play a skirmish and play one to the end.

Only the campaign loops and the tutorial are timed. The sandbox and the skirmish are there for your impressions and for bugs.

## Send your report

1. In the game, open Settings, then Playtest. Playtest timing log should be on (it is on by default).
2. Choose Copy timing log, then paste it into a [playtest report](https://github.com/kaileh57/black-pennant/issues/new?template=playtest_report.yml).
3. Back in the game, choose Copy operation log and paste that into the same form.
4. Answer the questions on the form. They take about two minutes.

The timing log holds, for each loop, when the briefing opened, the seconds spent in each step, the operation and its template, the difficulty and the result. The operation log holds each loop's times, the Salvage earned and spent, and how many ghost conditions you met. Neither holds your name or any file path, and the game never sends either one by itself.

When you quit, the game also writes all of it to `playtest_report.txt` in the logs folder, so you can file the report later.

## If the game crashes

At the next start the game says the last session did not close normally and names the logs folder. In Settings, under Playtest, Open log folder opens it. The crashed session's log is the newest file named `godot` followed by a date, for example `godot2026-11-21T23.42.05.log`; `godot.log` is the session running now. This file can contain your user name inside folder paths, so open it in a text editor and replace the name before you attach it. Attach that file to a [bug report](https://github.com/kaileh57/black-pennant/issues/new?template=bug_report.yml), with the steps that led to the crash. Copy diagnostics, on the same settings page, adds your renderer and changed settings without names or paths.

On Windows, `Black Pennant.console.exe` starts the same game with a window that shows the log as it runs.

## Where the game keeps your files

| | macOS | Windows |
| --- | --- | --- |
| Everything | `~/Library/Application Support/BlackPennant` | `%APPDATA%\BlackPennant` |
| Saves | `saves` in that folder | `saves` in that folder |
| Your ship designs | `blueprints` | `blueprints` |
| Replays | `replays` | `replays` |
| Logs | `logs`: `godot.log`, `timing_log.txt`, `operation_log.txt`, `tutorial_log.txt`, `playtest_report.txt`, `connection.log` | the same |

To start over as a new player, quit the game and delete that folder. Your designs and saves go with it.

## Known issues

These are known and need no report, though your notes on how much they hurt are welcome.

- **Campaign operations are hard to win.** In automated runs with the stock officers, the hunter group survival operation was won every time, but convoy raid was won once in four tries, and defector escort, agent extraction and the Act 1 final operation were not won at all. Most Act 1 operations have not been measured. The two ace duels may not be winnable, because aces never surrender.
- **The officers do less than you might expect.** The sensors officer keeps the radar off and the gunnery officer holds fire on contacts marked Suspect. Switch emissions on with E and designate targets yourself.
- **Most starter ships lose their trial fight.** Only the 22 m fast attack craft wins some (8 of 20); the other five starters lose all of theirs.
- **Contact labels can swap.** When two ships pass close together, their track numbers sometimes trade places.
- **Fit Check can miss a capsize risk.** In sea state 8, the picket and escort corvette starters can roll over in a hard port turn although Fit Check passes them.
- **Speed is not tuned for the minimum machines.** Frame rates on the minimum hardware have not been measured, and the simulation runs over its time budget in large battles. Your report's hardware line helps here.
- **Some screens are cramped at 2x interface scale.** In the tutorial's test basin the instruction card partly covers the ship, and the ship visibly settles into the water as the first test loads.
- **Number readouts use a stand-in font.** The monospace font the design calls for is not in the build yet, so figures and codes show in a system font.
- **No one has listened to the sound carefully yet.** Say if anything is too loud, too quiet or unpleasant.
- **The Windows build has never been run by a person.** It is built and smoke-tested by GitHub's Windows runner only. Windows reports are the most useful ones.
- **Saves may not open in a newer version.** Keep the old build if you want to finish a campaign.

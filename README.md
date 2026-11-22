# Black Pennant

Black Pennant is a naval combat game where you build the ship. Lay out a hull cell by cell in the dry dock, fit guns, sensors and engines, then take that one ship to sea against a fleet that outnumbers you. Officers run whatever you leave to them. You pick the targets and the course, and take any gun group or the helm by hand when you want it. Press Space in the dry dock and the design drops into a test basin, so you can fight a new idea as soon as it is built.

The game is free and open source. Builds are pre-alpha (version 0.x): expect rough edges, and saves that may not carry across versions.

## System requirements

- macOS 14 Sonoma or later, Apple M1 or later, 8 GB of memory.
- Windows 10 or 11, 64-bit: Intel Core i5-8400 or AMD Ryzen 5 2600, 16 GB of memory, and a graphics card with 6 GB of video memory (RTX 2060 or RTX 3050 6 GB class) and a Direct3D 12 or Vulkan driver.

Intel Macs are not supported.

## Download and run

Download the zip for your system from the [releases page](https://github.com/kaileh57/black-pennant/releases). Each release also carries `SHA256SUMS.txt` and a build-provenance attestation; `gh attestation verify <zip> --repo kaileh57/black-pennant` checks a download against it.

The builds are not signed with a paid certificate, so both systems warn you the first time you open the game.

### macOS

1. Unzip `black-pennant-<version>-macos-arm64.zip` and move Black Pennant to Applications.
2. Open Black Pennant. macOS says it cannot verify the app and refuses to open it; close that message.
3. Open System Settings, then Privacy & Security. Scroll down to Security, where Black Pennant is named, and choose Open Anyway.
4. Confirm with your password or Touch ID, then choose Open Anyway again.

On macOS 14 you can instead Control-click the app and choose Open. Each new version asks once more.

### Windows

1. Unzip `black-pennant-<version>-windows-x86_64.zip` into a folder. Keep the files together: the game needs `Black Pennant.pck` and `gdclient.dll` beside the executable.
2. Open `Black Pennant.exe`. SmartScreen shows "Windows protected your PC".
3. Choose More info, then Run anyway.

`Black Pennant.console.exe` starts the same game with a console window that shows its log. Attach that log when you report a problem.

Smart App Control blocks unsigned programs and has no per-app exception. It is on by default only on clean installs of Windows 11. If it blocks the game, the game cannot run on that machine until you turn Smart App Control off in Windows Security, under App & browser control.

## Controls

Every binding can be changed in Settings, under Controls, and every order at sea also has a gamepad binding.

### At sea

| Action | Key or mouse |
| --- | --- |
| Designate a target for the officers | Left-click a track |
| Set a course | Right-click the sea |
| Switch between the bridge view and the plot table | Tab |
| Speed ahead or astern | W, S |
| Rudder to port or starboard | A, D |
| Take the helm by hand | H |
| Take weapon group 1, 2, 3 or 4 | 1 to 4 (hold, or tap to toggle) |
| Take the sensors | 5 |
| Take the selected group | F |
| Next or previous weapon group | ] and [ |
| Next or previous track | . and , |
| Fire a group you hold | Left mouse button |
| Orbit the camera | Middle mouse drag |
| Look around | Arrow keys |
| Zoom | Mouse wheel |
| Pause | P |
| Faster or slower time | = and - |
| Acknowledge or silence an alert | Z, X |
| Cycle emission control | E |
| Fire decoys | C |
| Show the log | L |
| Cancel | `Backspace` |
| Menu | `Esc` |

### In the dry dock

| Action | Key or mouse |
| --- | --- |
| Test the design in the basin | Space |
| Rewind the test | R |
| Rotate a part (Shift turns it back) | R |
| Tilt a part | T |
| Mirror | M |
| X-ray view | X |
| Frame the ship | F |
| Delete | `Delete` or `Backspace` |
| Undo, redo | Cmd+Z, Shift+Cmd+Z |
| Copy, paste, duplicate | Cmd+C, Cmd+V, Cmd+D |
| Select all, save | Cmd+A, Cmd+S |
| Orbit, pan | Middle mouse drag; hold Shift to pan |

On Windows, use Ctrl where the table says Cmd.

## Play with friends

Multiplayer runs over UDP, straight between the players. There is no matchmaking server and no relay.

### Host a game

1. Choose Host or join a game and host a lobby. The game opens UDP port 47610 at this step, never earlier. Your system may ask whether Black Pennant can accept incoming connections: allow it, or nobody can join.
2. The game asks your router to open the port (UPnP, NAT-PMP or PCP) and the host panel says whether that worked.
3. Send the join code from the host panel to the other players. The code holds your public IP address, so share it only with people you want in the game. A new lobby key stops old codes from working.

Players on your own network can join with the LAN address the host panel shows.

### Forward the port by hand

If the host panel says the router did not open the port, forward it yourself:

1. Find this computer's LAN address. On macOS it is under System Settings > Network > your connection > Details. On Windows, run `ipconfig` and read the IPv4 address.
2. Open your router's admin page (often `192.168.1.1` or `192.168.0.1`) and find its port forwarding section.
3. Forward UDP port 47610 to that LAN address. TCP is not needed.
4. Host again. Players join with your public address and the port, for example `203.0.113.7:47610`, plus the lobby key from the host panel.

When the host panel says your network sits behind another one, port forwarding cannot help: your provider shares one public address between many customers, which is common on mobile and some fibre connections. Use a mesh VPN such as Tailscale or ZeroTier on every player's machine instead. With one running, the host panel offers a join code for the VPN address.

### Join a game

Choose Host or join a game, then paste the join code, or enter the host's address, port and lobby key. Joining needs no port forwarding. Every player needs the same version of the game; if versions differ, the message names both.

### Dedicated server

The headless server runs the same simulation without Godot:

```sh
cargo run --release -p server -- serve --port 47610
```

`--config server.ron` reads a dedicated server config, and `--no-map` skips the router port mapping. `cargo run -p server -- help` lists every option.

## Build from source

You need:

- Rust through [rustup](https://rustup.rs). The toolchain pinned in `rust-toolchain.toml` installs itself on first use.
- [Godot 4.7.2](https://godotengine.org/download/archive/), the standard build rather than the .NET one, on your PATH as `godot` (or set `GODOT` to its path).
- On Windows, the Visual Studio Build Tools with the "Desktop development with C++" workload, which Rust uses to link.

Run the game from a checkout:

```sh
cargo build -p gdclient          # the Rust library that Godot loads
cargo xtask gen                  # generated shaders, textures and sounds, never committed
godot --headless --path game --import
godot --path game
```

The first import of a fresh checkout prints errors while the Rust classes register. Run the import a second time and it is clean.

Make a release build for the system you are on:

```sh
cargo xtask release
```

That builds the release library, runs the generators, exports with Godot, ad-hoc signs the macOS app and checks the signature, and writes the zip to `target/bp-release/`. The export needs the Godot 4.7.2 export templates, which the editor installs from Editor > Manage Export Templates. On macOS the export opens a Godot window for a few seconds so it can bake shaders for Metal; `--no-bake` skips that.

Pushing a tag such as `v0.1.0` or `v0.1.0-beta.1` runs the same command on GitHub's hosted macOS and Windows runners and publishes the zips, with checksums, as a release (a pre-release when the tag has a hyphen).

Tests run headless:

```sh
cargo test --workspace
cargo xtask lint
```

## Where the game keeps your files

Settings, saves, blueprints and replays live in your user data folder:

- macOS: `~/Library/Application Support/BlackPennant`
- Windows: `%APPDATA%\BlackPennant`

The game sends nothing unless you choose Send on the crash report prompt. Settings, under Privacy, changes that choice.

## How it was made

Code, text and procedural assets were written by an AI coding agent under the owner's direction. No generative image, audio, voice or music models were used. Every ship, sea and sky is drawn by code, and every sound is synthesized.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) first. Bug reports, playtest logs and blueprints go to GitHub issues.

## Licences

- Code: [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
- Game data, stock blueprints and kits: [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
- The Atkinson Hyperlegible Next typeface: SIL Open Font License 1.1, see [NOTICE.md](NOTICE.md).
- Godot Engine: MIT. The in-game credits list every third-party licence.

Blueprints you make belong to you, under the licence written in their header.

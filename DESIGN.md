# Black Pennant design spec

**Frozen 12 November 2026.** Every design decision is closed. Items that can only be settled by measurement carry a default, a pass/fail gate and an automatic fallback. Any change after the freeze is an owner decision, recorded in this file with its date.

# Black Pennant design doc

Nov 12, 2026 · @Kellen Heraty

Status: frozen. The spec was frozen on 12 November 2026. Every design decision is closed, and every item that can only be settled by measurement carries a stated default, a numeric pass/fail gate with its measurement method, and an automatic fallback. Changes after the freeze are owner decisions.

## Summary

Black Pennant is a naval combat sandbox. Players design warships in an editor, then take one elite vessel against an enemy navy that outnumbers them many times over. It takes the build-it-yourself appeal of Stormworks and From the Depths and removes the wiring and logic work that stops most players before their first fight.

The target player has bounced off a vehicle builder's logic system, or enjoys naval combat games but never built anything. The expert builder stays in the audience: every automatic system opens up into the same graph and scripting tools that experts would otherwise build by hand.

The genre has a platform gap. In a Steam check of ten comparable builders, only From the Depths, Besiege and Stormworks ship on macOS, and Geometa's newest builder, Anymaker, is Windows-only ([Anymaker on Steam](https://store.steampowered.com/app/4435340/Anymaker/)). Black Pennant ships native on Apple Silicon (arm64, macOS 14 or later) and Windows (x86\_64) from the vertical slice (milestone M1) onward.

The rest of the doc depends on these eight decisions.

| Decision | Choice | Main reason |
| --- | --- | --- |
| Who builds it | Claude, an AI coding agent, builds nearly all of the game full-time. The owner directs, reviews and playtests. There is no human team, no contractors and no hiring. | Milestones end on measured gates, not dates. Calendar durations are estimates for an agent working continuously, gated by owner review and playtest time. |
| Distribution and licence | A public, open-source GitHub repository. The first public release (version 0.x) and every later build go out free on GitHub Releases for macOS arm64 and Windows x86\_64. Steam is phase 2, taken only if the owner decides the game is really good, and the game is free there too. Code is MIT OR Apache-2.0, data and blueprints CC BY 4.0, fonts OFL. | Cash spend is zero. The only possible cost is the one-time $100 Steam Direct fee if Steam happens. |
| How automation works | The editor generates a ship control graph from the parts you place. Players can tune, pin or replace any node in it. | One mechanism serves the newcomer and the expert, so there is no second code path to drift out of date. |
| Where the game runs | A pure Rust simulation with no Godot dependency. Godot 4.7.2 renders it through a thin godot-rust adapter (gdext 0.5.5). | The sim is tested headless on free GitHub-hosted Windows, Linux and macOS runners on every pull request, which stands in for the Windows PC the project does not own. |
| Multiplayer model | Server-authoritative state sync over renet2 on UDP. The host maps its port with UPnP IGD or NAT-PMP/PCP, and players join by address or join code. Single-player runs the same server in-process over a loopback transport. | Lockstep needs bit-identical floats across Apple Silicon and x86, which cannot be checked by hand without an owned x86 machine. State sync tolerates the difference. |
| Campaign | Task Unit LANTERN against the Meridian Combine in a persistent island theatre, four acts and 40+ hours. The first public release ships Act 1. | Acts are data on top of shared systems, so content grows without new code. |
| Online services | None run by the project. Ships are shared as blueprint files and copyable share strings. Crash reports go to Sentry's free Developer plan, opt-in on first launch. Steam auth, Workshop, Cloud saves and Steam Datagram Relay sit behind a cargo feature, off by default, and turn on only in phase 2. | Zero spend and zero operations work. Blueprint formats and the transport trait stay store-agnostic, so the GitHub and Steam builds share one code path. |
| Art and audio | Made in code only: procedural meshes and shaders, hand-written SVG icons, in-engine renders, and sound synthesized in Rust. No generative image, audio, voice or music models and no third-party asset libraries, CC0 included. The look is flat-shaded, faceted steel in tonal naval greys under a low warm dusk sun, a smooth lit sea matched to the simulation, and no outlines (see Graphics and rendering). | Every asset lives in the repository as code that Claude can build, change and regenerate, with no licence or provenance question. |

Black Pennant is the final title, and LANTERN (the task unit), SEXTANT (its tender), Saltreach (the archipelago) and the Meridian Combine (the enemy) are final names. A knockout search on 12 November 2026 found no live mark identical to BLACK PENNANT for games (Nice classes 9, 28 and 41) at the USPTO ([USPTO Trademark Search](https://tmsearch.uspto.gov/search/search-information)) or at the EUIPO, the UKIPO or WIPO Madrid ([TMview](https://www.tmdn.org/tmview/)). The nearest mark in meaning is Ubisoft's BLACK FLAG for games. Unregistered use was not checked, and no paid clearance opinion or filing is made, because cash spend is zero. If the game goes to Steam, the same search runs again before the store page. Gate: no live mark identical to BLACK PENNANT for games in classes 9, 28 or 41, and no written claim against the title received by the project. Fallback: if either fails, the game is renamed Wolfwater, for which the same search found no live identical mark for games; the fiction names do not change. Wolfwater takes the same gate, and a rename beyond it is an owner decision.

## Pillars and non-goals

Six rules settle design arguments. When two features compete for build time, the one that serves a higher row wins. A feature that serves several rows counts at its highest row.

| Pillar | The rule | What it rules out |
| --- | --- | --- |
| A warship in ten minutes | A first-time player builds a vessel that floats, steers and shoots in one short session, without connecting anything by hand. | Mandatory wiring, plumbing or logic before the first fight. |
| Automatic by default, open all the way down | Every automatic behavior is a visible default that the player can tune, rewire or script. | Hidden magic, and features that exist only in automatic mode. |
| Combat is the point | A system earns its place by making fights better to plan, read or win. | Rescue work, cargo hauling and simulation detail that never affects a fight. |
| One against many | The player is outnumbered in every campaign operation: the enemy commits at least five times the player's hull count. Design, preparation and tactics beat numbers. | Fair duels as the main course, and a simulation that slows down before the reference battle's 40 full-physics enemy hulls. |
| Multiplayer is the same game | Single-player runs on the multiplayer code path. A feature ships when it works with two players. | Single-player-only systems that get bolted onto the network later. |
| Same game on Mac and Windows | Both platforms run identical simulation code. Every pull request runs the simulation tests and the trial battery on free GitHub-hosted Windows and macOS (arm64) runners. Windows rendering is tested on hosted runners with software rendering (Direct3D 12 WARP, and Vulkan and OpenGL through Mesa lavapipe and llvmpipe where available). GPU performance is gated on the owner's Apple Silicon Mac under a throttle profile that approximates the base M1 minimum spec, and community testers report real Windows and Mac GPU results from the in-game benchmark scene. | Platform-specific features, and fixes that only one platform receives. |

Each pillar with a number has a measured gate in the section that owns it. Time to first fight (10-minute median, from opening the tutorial to the first splash where the player's guns fire) and its fallbacks are in The editor. The hull-count and tonnage multiples and their generator check are in Core loop. The reference battle and the minimum swarm of 20 full-physics hulls, below which the full-physics cap never shrinks, are in Performance budgets. PvP and skirmish are the only modes where sides can be even. Pause is the one single-player-only control, because a shared simulation cannot stop for one player; co-op gets time compression by unanimous hold instead (see Core loop).

The game leaves these out on purpose:

- A walk-around character, and vehicles crewed by several players. One person runs each vehicle, and officers fill every other role. That removes on-foot movement, interiors, station handoff between players and character netcode from scope, and it means every vehicle must be fully playable by one person.
- Land vehicles, and crewed aircraft that the player builds. Aircraft appear as enemies, missiles and drones.
- Real-world ship classes, weapon names and geography. The setting is fictional.
- A Linux or console client, in the first public release or at 1.0. Linux is a target for the headless dedicated server only.
- Collaborative editing of one blueprint by several players, in the first public release or at 1.0.
- Enemies that remember individual player actions. Adaptation is fleet-level doctrine cards, and ace returns are authored and fixed (see Campaign).
- A damage difficulty slider. Damage is the same physical model at every difficulty.
- Editing ships with a gamepad. Sorties, menus and the chart table are fully playable on a gamepad; the editor stays keyboard and mouse.
- Voiced dialogue, in-sortie music, and any art or audio not made in code. Officers and aces speak in text with radio effects. No generative image, audio, voice or music models and no third-party asset libraries, CC0 included (see Graphics and rendering, and Art and audio).
- Paid content. Every release is free, including every theatre after 1.0 and the game on Steam if it goes there.

## Core loop

The loop runs about 30 to 45 minutes from one briefing to the next. Design work happens inside the loop on every mission rather than once at the start, because each briefing changes what a good ship looks like.

1. Read the briefing. It gives sea state, distance, a threat picture graded by confidence, and two to four constraint tags such as a draft limit or a radar cross-section cap. About 1 minute.
2. Refit in the editor. Fit Check scores the current design against the briefing as pass or fail per tag, and the Quartermaster can propose a legal refit with a diff of every changed part, or says "No legal refit found" and names the blocking tag. 2 to 10 minutes.
3. Splash test. Press Space to put the ship in a test basin (p95 of 1 s or less as the target, 2 s as the hard limit, measured as in Instant testing), fight a drone or a patrol boat, rewind, and adjust. Optional, 1 to 5 minutes.
4. Sortie. Fight the operation against at least five times the player side's hull count and at least four times its tonnage (see Enemy commitment below). 15 to 30 minutes.
5. Recover. Salvage what you sank, take prizes, repair at the tender, and read the after-action report. It shows what the Combine learned about you at fleet level: the win-method tallies its doctrine cards track, any counter now queued on the doctrine bulletin, and the task-group belief-map overlay. No individual enemy remembers your actions. 2 to 5 minutes.
6. Advance the theatre. Choose the next transit, refit or wait on the chart table. Theatre time runs in one-hour steps until the next decision or interrupt, and the enemy replans. In co-op a proposal commits when all players accept or the host confirms. 1 to 3 minutes.

Then the next briefing arrives with new constraints, and the loop returns to step 1. Outside the campaign the same loop runs without steps 5 and 6: build, splash, fight a skirmish or a PvP match, iterate. Sandbox, skirmish and PvP have every part from first launch. PvP limits come from the lobby ruleset (open or standard; standard is the default and bans forked officers and graph edits), not from an unlock track. At the first public release (GitHub, version 0.x) PvP is elimination at 1v1 and 2v2.

### Loop and sortie length

The step budgets add up to 20 to 46 minutes for steps 1, 2, 4 and 5, plus 1 to 5 minutes of optional splash and 1 to 3 minutes at the chart table. The 30 to 45 minutes is a target median, not the sum of the extremes.

- Targets. Median loop 30 to 45 minutes, measured from opening one briefing to opening the next. Median sortie 15 to 30 minutes. Both drop any pause longer than 2 minutes and any time spent suspended.
- Measurement. The game writes a local playtest timing log in the user data folder: per loop, the briefing-open times, the time in each step, the sortie length, the operation template, the difficulty preset and the first-attempt outcome. Nothing is sent anywhere. Settings > Playtest > Copy timing log puts it on the clipboard as plain text for the playtest issue form on GitHub. No telemetry service collects it.
- Proxy gate (headless CI on the free hosted runners, every content change). The stock-officer check (every Act 1 operation winnable with stock officers on Normal) runs each Act 1 template and story operation on 20 seeds. For each one, the median simulated sortie length must be 15 to 30 minutes and no run may exceed 45 simulated minutes. Any operation that fails either clause blocks the merge.
- Player gate, vertical slice. At least 10 playtesters (the owner plus community volunteers on GitHub pre-release builds), with 3 or more Act 1 loops each. It passes when the median sortie is 15 to 30 minutes and the median loop is 45 minutes or less. A median loop under 30 minutes is not a failure.
- Player gate, first public release. The same check is re-run on at least 200 loops from GitHub pre-release builds.
- Small samples. If a player gate is due with fewer testers or loops than it needs, it runs on every shared loop, provided there are at least 30 loops from at least 3 players, the owner included. Below that, the proxy gate alone decides, and the player gate re-runs at each 0.x update until it has the full sample.
- Fallback, applied automatically in this order, once per gate run, and re-measured after each step:
  1. If the median sortie runs over 30 minutes, the operation generator cuts the approach distance (player spawn to first objective) by 25%. The enemy commitment minimums still apply.
  2. If the median refit step runs over 10 minutes, the briefing opens with the Quartermaster's proposal already computed and shown.
  3. If the median sortie runs under 15 minutes, the approach distance grows by 25%.

  Approach distance stays between 50% and 150% of each template's authored value. If the medians still miss after step 3 at the first public release candidate, the release ships, the measured medians replace the targets in this section, and the generator change carries into the next gate run.

### Enemy commitment

Every operation commits at least 5 times the player side's combat hull count and at least 4 times the player side's tonnage. Player-side tonnage is the act's hull cap times the number of player hulls (150, 400, 900 or 1,800 t per hull), not the hulls' actual displacement, so building small does not shrink the enemy. Each player commands exactly one combat hull, so a solo Act 1 operation commits at least 5 hostile hulls and 600 t, and each added co-op player raises both minimums by the same amounts. Committed means hostile hulls present at start plus scheduled reinforcements. Enemy capitals over 90 m (up to 150 m, at most 4 per operation) count toward both multiples and toward the reference-battle budget.

- Gate 1 (headless CI on the free hosted runners, every content change). The operation generator builds 200 seeded instances of every template in every act and for every player count, and 100% must meet both multiples.
- Gate 2 (vertical slice, then each 0.x update; at least 10 players, Normal difficulty, read from shared timing logs). The first-attempt success rate on Act 1 template operations is between 40% and 80%. With fewer than 10 players or fewer than 50 first attempts, the gate is not scored, the tonnage multiple stays at 4x, and the gate re-runs at the next 0.x update.
- Fallback:
  1. If the generator cannot meet a multiple from the region's order of battle, it adds a reinforcement wave from the nearest forward base. CI fails the template if even that falls short.
  2. If the success rate is under 40%, the tonnage multiple drops to 3x. If it is over 80%, it rises to 5x. It changes once per gate run and stays between 3x and 5x.

  The 5x hull-count minimum never changes, because it is the One against many pillar.

### In battle

A sortie has two views, and Tab switches between them. There is no continuous zoom between them. The bridge view is an orbit camera around your ship that stays at least 1 m above the sea surface (the first public release has no underwater rendering), with the track list docked at the left edge and the Bridge Board, which stays docked in both views, at the right. A setting swaps the two edges. The plot table is a 2D chart showing tracks with their uncertainty shapes, weapon envelopes, enemy emitters and your planned route.

Orders work the same in both views. Click a track to designate it, and the officers do the rest. Right-click the sea to set a course. Hold a mount's key to take it by hand, and release to give it back. A press shorter than 250 ms toggles the takeover instead, until the key is pressed again, and a setting turns every hold into a toggle (accessibility floor 8). Every sortie order also has a gamepad equivalent: plot cursor, designation, helm, mount takeover, view switch, pause and time compression.

Time rules by mode:

- Single-player can pause at any time. Designations and course orders given while paused take effect when time resumes; graphs stay editable only in dry dock or during a splash. Quitting mid-sortie writes a suspend save from the 60 s operation checkpoint to the one suspend slot, and loading it deletes it (see Campaign).
- Time compression has three rates: 1x, 2x and 4x. In single-player the rate the player sets is the effective rate. It is available only while no inbound weapon and no track held by the player side whose stance is Suspect or Hostile is inside the longest weapon envelope of any player hull, and no player hull is inside the weapon envelope the plot table draws for any such track. Friend and Neutral tracks (co-op partners, SEXTANT, escorted ships) never block it. The test uses only what the player side knows, so compression never drops because of a contact the player has not detected. The moment the test fails, the rate drops to 1x.
- In co-op each player sets their own requested rate, a toggle by default or a momentary hold in settings. The effective rate is the lowest rate any player requests, so compression engages only while every player requests it and drops to 1x the moment any player releases. Co-op never pauses.
- PvP has neither pause nor compression.
- Compression is capped by the host's simulation budget; in single-player the host is the in-process server. Under load, compression steps down from 4x to 2x to 1x before any unit moves to coarse simulation, and it is unavailable while the time scale is degraded below 1x by the overload rules. The HUD always shows the effective rate.

## The editor

A first-time player gets a ship that floats, steers and hits things in about ten minutes, because the editor starts from a working hull and every part connects itself. The vertical slice proves the figure with real playtesters against the two gates in The first ten minutes below.

### Hull and scale

A hull starts as a lofted shell: 4 to 16 cross-section stations, mirrored by default, with drag handles for beam, draft, deadrise, flare, sheer, bow rake and transom. Every handle also has a numeric field, so no edit needs a drag.

Player hulls run from 10 m to 90 m (about 100,000 cells at 90 m) in every mode: campaign, sandbox, skirmish and PvP. Combine classes are built in the same editor with its enemy-class mode, which allows up to 150 m. At most 4 hulls over 90 m appear in one operation, and they count against the reference-battle budget. Player modes refuse to load a blueprint longer than 90 m and say why.

The shell voxelizes into 0.5 m cells. Boundary cells take a wedge, corner or inverse-corner shape, stored as a per-cell shape and rotation byte, so curved hulls read as faceted steel rather than a staircase; the sim may treat shaped cells as full cells (shape set and voxelizer rule in Graphics and rendering). The player paints armour onto the outer cells in four tiers (5, 15, 40 and 80 mm).

Hull paint uses a ship palette of 16 entries, the 4-bit swatch field of the hull volume. Eight entries are the US Navy swatches from Graphics and rendering, and they are the default. The other eight hold custom colours from a picker. Hull colours clamp to linear Y 0.6 or below and OKLCH chroma 0.06 or below. Markings (stripes, flags, hull numbers in MSDF text) clamp to Y 0.7 or below with no chroma clamp. Decks default 3 or more swatch steps darker than the sides. The editor adds a 1-cell hull.black boot-topping band at the design waterline, with red-brown antifouling (Y 0.08) below. Weathering is at level 0 in the editor.

Decks (2.5 m apart by default) and bulkheads are planes you drag through the hull, and they cut it into compartments for flooding and routing. Superstructure uses the same cell shapes (cube, 45 degree wedge, corner and inverse corner in 24 orientations) plus placeable 1:2 slopes. Functional parts occupy whole cells, so damage, routing and connectivity all work on the one 0.5 m grid.

Experts can bake a shell into freeform cells and sculpt it cell by cell. The bake is one-way: Sprocket keeps generated and freeform designers side by side with no lossless conversion between them, and recovering parameters from an edited mesh is a research problem ([Sprocket patch notes](https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid=1674170&count=300&maxlength=0)).

### Parts and kits

Parts have typed ports and no wires. A part joins its compartment's network as soon as you place it, and the editor animates the conduit it routes (see Automatic systems below).

Kits are versioned sub-assemblies with named sockets and a settings sheet: a twin 76 mm mount with its magazine and hoist, a radar mast with its fire-control director. A kit carries its own graph fragment, so a placed kit already knows how to aim and fire. Updating a kit offers to migrate every placed copy. The first public release ships about 60 parts in 5 tiers and about 30 kits (catalogue and unlock order in Campaign). Sandbox, skirmish and PvP offer every part from first launch; PvP limits come from lobby rulesets.

Six starter hulls ship with the game, each already afloat and fitted, as blueprint data in the repository under CC BY 4.0. Sandbox, skirmish and PvP offer all six from first launch. In the campaign each act offers its own:

| Act (tonnage cap) | Starter hulls |
| --- | --- |
| Act 1 (150 t) | 15 m patrol boat; 22 m fast attack craft; 26 m low-signature insertion craft |
| Act 2 (400 t) | 40 m missile corvette |
| Act 3 (900 t) | 60 m corvette with mine rails and boarding kit |
| Act 4 (1,800 t) | 90 m destroyer |

The 26 m insertion craft gives Act 1's stealth-insertion briefings, which carry a cross-section cap tag, a stock answer without a redesign in the first hour. The first starter a player meets is the 22 m fast attack craft. CI fails any starter hull that exceeds the cap of the act that offers it.

### Live feedback

A gauge strip along the top recomputes within 100 ms of each edit, except Complexity, which updates after each save:

| Gauge | What it shows |
| --- | --- |
| Float | Displacement against the current tonnage cap, reserve buoyancy, metacentric height (GM), the righting-arm (GZ) curve and range of stability |
| Speed | Top speed in knots and turning circle |
| Power | Cruise and combat power balance |
| Endurance | Range at cruise on current fuel |
| Punch | Weight of fire per minute and effective range per weapon |
| Sight | Radar horizon against three reference targets (sea-skimming missile at 5 m, ship mast at 20 m, aircraft at 1,000 m), using 4.12 x (sqrt(antenna height) + sqrt(target height)) km with heights in metres, and coverage by sector |
| Resilience | Survival with one and two compartments flooded, and the share of systems that survive a random hit |
| Complexity | Complexity points from the trial battery's 10 s headless trial (about 1 single-core microsecond per 60 Hz tick each); Caution above 400; the PvP cap of 600 per vessel is marked (definition in Multiplayer) |

GM shows Fail (a cross in alert.warning) at zero or below, and Caution (a triangle in alert.caution) below 4% of beam and again above 15% of beam, where the roll gets snappy. Overlays draw the waterline and heel, centres of gravity and buoyancy, weapon arcs on the water (hatched in alert.warning where your own structure blocks them), sensor blind sectors, conduit routes and a vulnerability heatmap.

GM thresholds gate: at the vertical slice and again whenever the buoyancy model changes, the trial battery builds a sweep of 30 hull variants from the 6 starter hulls, with ballast moved so GM runs from 1% to 25% of beam, and runs each through two tests:

- Tender test: a beam sea at Beaufort 7 for 10 minutes. A variant fails if it heels past 30 degrees or capsizes.
- Stiff test: a beam sea at Beaufort 4. A variant fails if peak roll acceleration at the highest mount exceeds 0.3 g, or if gun dispersion grows by more than 25% over the median variant.

The thresholds pass if every variant below 4% fails the tender test, every variant above 15% fails the stiff test, and no variant between 4% and 15% fails either. Fallback: CI moves each threshold to the GM-to-beam ratio where the sweep's failures actually begin, rounded to the nearest 0.5% of beam, and writes the new values into the versioned tuning table. The gauge prediction keeps the 10% drift rule.

Gauge latency is measured from the edit commit to the gauge redraw, on a scripted replay of 200 edits on each of the 6 starter hulls and on a 90 m stress hull of about 100,000 cells. Three checks use it:

- Release gate, at every vertical-slice tag and every release tag: on the owner's Apple Silicon Mac under the min-spec throttle profile (Platform support), p95 must be 100 ms or less.
- Pull-request check on the free GitHub-hosted runners: the headless Rust part of the replay (edit commit to gauge values ready). A pull request fails if stress-hull p95 rises more than 20% above main's last nightly on the same runner image. Hosted-runner timings are compared only with themselves.
- Community check: the in-game benchmark scene has an editor stage that runs the same replay and reports p95, so Windows minimum-spec results come from community reports.

Fallback, automatic:

1. Split strip. Float, Speed, Power and Punch must still meet 100 ms. Resilience (enumerating one and two flooded compartments), Sight sector coverage and Endurance move to the background worker. Until a fresh value arrives, a gauge draws its previous value in text.secondary with a stale glyph and the word "Updating", and it must refresh within 1 s p95. If the release gate fails, split strip becomes the shipped default. On any machine, the editor also switches to split strip by itself when p95 over the last 50 edits exceeds 100 ms, and switches back after 50 consecutive edits under 70 ms.
2. If Float still misses 100 ms, the GZ curve is computed at 10-degree heel steps while a handle is being dragged, and at the full 2-degree resolution on release.

Ship's Notes is a ranked problem list in plain sentences, such as "Gun 2 cannot reach targets ahead: the bridge blocks 340 to 020 degrees." Each note has a Fix button that previews its change as a ghost before you accept it. Open notes add a Caution triangle and the open-note count to the Launch button, but never block it, because blocking kills experimentation.

Notes rank first by severity tier, highest first:

1. The ship cannot float, move or make power (Float Fail, no propulsion, zero generation).
2. The design fails a constraint tag in the current briefing's Fit Check (campaign only; empty elsewhere).
3. A weapon, sensor or consumer cannot work: an arc blocked by own structure over 50% of its sweep, unpowered, unconnected, or a pinned node that lost its input.
4. A gauge is at Caution.
5. Advisory.

Within a tier, notes whose Fix flips a Fit Check tag from Fail to Pass come first, then notes that affect more parts, then ascending part index, so the order is deterministic. Gate (vertical-slice playtest, at least 10 testers, 5 seeded flawed designs each): each tester picks the most important problem before seeing Ship's Notes, and the top note must match the pick in at least 70% of cases. Fallback: the within-tier order becomes each note's measured effect on the trial battery's one-against-three score, comparing the design with and without the Fix, computed by the background worker after each save. The tier order stays.

The editor auto-sets a value only when a headless test proves the automatic value correct. Elsewhere it shows the manual control with a Suggest button. Sprocket removed its automatic cannon rotation limits because the calculation "didn't work very well", and Ultimate Admiral: Dreadnoughts spent much of 2023 and 2024 patching its auto-designer ([UA:D announcements](https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid=1069660&count=400&feeds=steam_community_announcements&maxlength=0)).

Auto-set at launch, each behind a named property test:

1. Conduit routes: every consumer is connected, two paths wherever the adjacency graph allows, magazines and waterline avoided, and the same design always routes the same way.
2. Default power priorities (the table in Automatic systems, gated by its power-starved trial).
3. Mount training and elevation limits: a ray sweep at 1-degree steps over each arc. A value qualifies only if no sampled shell path hits own structure and no clear sector is excluded.
4. Wiring inside a kit (magazine, hoist, gun, director).
5. Ballistic solver tables from the projectile drag model.

Suggest at launch: ballast and trim, armour layout, engine and generator sizing, fuel load, sensor placement, bulkhead placement, the doctrine Aggression preset and the EMCON trigger range.

Qualifying test, run in CI on every pull request: the property test is 100% clean across a corpus of the 6 starter hulls, every enemy class and 200 seeded mutated variants, and the trial battery score with the automatic value is within 2% of a hand-set reference value. Fallback: any auto-set value whose test fails on any corpus member drops to Suggest for that release automatically. It shows the manual control with a Suggest button and a Ship's Note naming the failing case, and it is promoted back only after 30 consecutive clean CI days.

### Instant testing

Splash (the Space key) drops the ship into a test basin. Pick calm water, Beaufort 4 or Beaufort 7, and a target: drones or a patrol boat that fights back. During a splash you get a 30-second rewind, a scope on any port, and live edits to kits, tuning and graphs. Changing the hull shell returns you to dry dock. Every splash ends with a report card and one tip. In a splash, a gamepad drives the helm and mount takeover as in a sortie.

Space saves the design, and Splash builds its basin world from the collider compound and mass properties that the save caches. Load time is measured from the Space press to the first frame with helm input accepted: 20 runs each on the 22 m fast attack craft, the 90 m destroyer starter and the 90 m stress hull, cold (first splash of a session) and warm. The target is p95 of 1 s or less, and the hard limit is p95 of 2 s or less. The release gate runs at every vertical-slice tag and release tag on the owner's Mac under the min-spec throttle profile. The hosted runners time the headless world build on every pull request with the same 20% relative rule as the gauges, and the benchmark scene's editor stage reports splash times from community machines.

Fallback, automatic, in order:

1. Pre-built splash. While the editor is idle for 500 ms, the background worker keeps a ready splash world in sync with the design, so Space only swaps worlds. It becomes the shipped default if the release gate misses the 1 s target. On any machine, a splash over 1 s turns it on for that machine, and the choice persists in user settings.
2. Progressive splash. The ship and calm water appear first and accept helm input, and the sea state and targets arrive within the next 1 s. It becomes the shipped default if pre-built splash still misses the 2 s hard limit, and turns on by itself on any machine where a pre-built splash exceeds 2 s.

There is no hull size cap below the 90 m player limit.

On each save, a background Rust worker runs a trial battery: turning circle, acceleration, heel recovery and a one-against-three fight. The fight puts the design against three Combine hulls drawn by fixed seed from the enemy classes of the lowest act whose tonnage cap the design fits (the heaviest act with classes in the build if it fits none), for 180 simulated seconds at Beaufort 4. Its score is 100 times the share of enemy tonnage disabled, minus 50 times the share of own cells lost, averaged over seeds. A save runs 3 seeds and CI runs 20. Results land in a scorecard and a Compare view against earlier versions, within 60 s of the save at p95 on the owner's Mac under the min-spec throttle profile. Fallback: if that gate fails at a tag, the on-save fight drops to 1 seed and 90 simulated seconds, the scorecard marks it "Short trial", and CI keeps the full battery.

The same battery is the CI regression suite on the hosted Windows and macOS runners, and it fails when a gauge prediction drifts more than 10% from the simulation.

### Ergonomics baseline

These ship in the vertical slice (M1), because players treat their absence as bugs: mirror and radial symmetry, drag-to-place, multi-select with copy and paste, one undo history across hull, kits, tuning and graphs, prefab capture with version and author lineage, blueprint folders, per-ruleset part limits, and remappable keys with one default scheme. Ships are shared as blueprint files and as copyable share strings (zstd-compressed, base64-encoded RON) that import by paste; Steam Workshop is a phase 2 feature. Every drag has a non-drag alternative: numeric handle fields, click-to-place and Shift-click selection. The editor is keyboard, mouse and trackpad only; gamepads work in splashes, sorties, menus and the chart table. Shortcuts use Cmd on macOS where Windows uses Ctrl.

On macOS the editor is fully usable on a trackpad alone. None of the genre peers checked has a trackpad-first editor to copy, so the mapping follows Blender's documented trackpad defaults and players' habits carry over ([Blender touchpad](https://docs.blender.org/manual/en/latest/getting_started/configuration/hardware.html)):

- Two-finger drag: orbit around the point under the cursor at gesture start, or the selection centre if nothing is under the cursor.
- Shift + two-finger drag: pan.
- Pinch: zoom toward the cursor.
- Cmd + two-finger vertical drag: zoom.
- Settings: a Two-finger drag: Orbit / Pan switch, as in SketchUp ([SketchUp trackpad](https://help.sketchup.com/en/trackpad-controls)); when set to Pan, Shift + two-finger drag orbits. Separate invert toggles for orbit, pan and zoom, and a speed slider for each.
- Control is never a gesture modifier: Ctrl-click is macOS secondary click, and Ctrl + scroll is the system zoom shortcut.
- Primary click (physical click or tap-to-click) selects. Shift-click adds to or removes from the selection. Click-drag on empty space draws a box select. Click-drag on a part moves it.
- Placing: a kit dragged from the tray, or picked with a click, follows the cursor as a ghost that snaps to cells and mirrors. A click places it and keeps placing until Esc. R and Shift+R turn it 90 degrees about the vertical, and T tilts it.
- Secondary click (two-finger click or tap, or Ctrl-click) opens the context menu that holds Why? and, during a splash, course orders.
- Three-finger gestures belong to macOS and are not used. If the player has turned on Three Finger Drag, it works as an ordinary click-drag.

Godot 4.7.2 limits the mapping. macOS sends two-finger drags as InputEventPanGesture (with modifier state) and pinches as InputEventMagnifyGesture; there is no rotate gesture and no momentum flag, so the camera adds no inertia of its own ([Godot macOS input source](https://github.com/godotengine/godot/blob/4.7.2-stable/platform/macos/godot_content_view.mm)). Godot's Windows display server sends neither event, so on Windows a touchpad behaves as a mouse wheel and uses the mouse scheme. Windows touchpads get no further support or testing from the project.

Trackpad gate, at the M1 exit: at least five testers on Apple Silicon MacBooks with no mouse attached run The first ten minutes steps 1 to 6 plus two camera tasks: show the gun arc from below the waterline, then frame the stern. Testers are spread across a 2x2 of tap-to-click (on, off) and natural scrolling (on, off). Each run logs completion time, camera resets, an orbit followed by an opposite orbit within 1 second (a wrong-gesture proxy), any request for a mouse, and a 1 to 5 comfort rating. Pass: every tester finishes without camera help, nobody asks for a mouse, and median time is no more than 1.25 times that of a mouse-using baseline group. A failed run blocks the M1 exit. CI replays recorded PanGesture and MagnifyGesture events, with and without modifiers, through the headless editor camera controller to lock the mapping.

### The first ten minutes

1. Open the tutorial variant of the 22 m fast attack craft: afloat but under-beamed, unarmed and without radar. The fully fitted version is the reward for finishing.
2. Drag the beam handle until the Float gauge shows Pass.
3. Drop four kits from the tray (a gun, a radar mast, an engine, a fuel tank) and watch the conduits route themselves.
4. Apply the Fix that Ship's Notes offers for a blocked gun arc.
5. Splash. Steer with WASD while the guns track and hit the drones on their own.
6. Beat the patrol boat, move the Aggression slider, and splash again.

Two gates measure it, because first fight and first kill are different events:

- Time to first fight: from opening the tutorial to the first splash where the player's guns fire at the drones (step 5). Pass: median 10 minutes or less, the Pillar 1 figure.
- Time to first splash kill: to the first enemy destroyed in a splash. Pass: median under 15 minutes, and at least 80% of testers within 20 minutes.

Measurement. Milestone 0 spike 6 records a baseline for both with 5 to 10 genre newcomers on the paper-thin prototype, with no pass mark. The tutorial writes step timestamps to a local log, and nothing is sent automatically. The vertical-slice gate needs at least 10 genre newcomers recruited by the owner, in sessions the owner moderates in person or by screen share, and both gates must pass. The first public release then needs both gates to pass again on at least 30 community runs of the vertical-slice or later builds from GitHub Releases, whose testers attach the log to a GitHub issue form.

Fallback, applied in this order with a re-test after each:

1. The tutorial hull starts with Float already at Pass, and step 2 becomes a 20-second demonstration of the beam handle.
2. The engine and fuel tank start fitted, so the player drops 2 kits (gun and radar mast) instead of 4.
3. The blocked-arc Fix is applied automatically, with the ghost preview played as a demonstration.
4. Last, the tutorial becomes one drag: the fully fitted variant minus its gun, drop the gun kit, and splash.

Pillar 1 keeps its wording, and the measured medians are reported in each milestone review.

## Automatic systems and control layers

Every vessel compiles into one ship control graph, and the defaults are nodes in that graph rather than hidden code. A newcomer never sees the graph. An expert can open it, change one node, and keep everything else automatic.

### Three ship-wide networks

Parts connect through three networks: Power (kW), Fuel (L/s) and Data. Track and command messages travel on Data. Ammunition stays inside each mount kit (magazine, hoist, gun), so kits stay self-contained. Coolant and hydraulic networks are cut. They add plumbing for newcomers and simulation code to build and test without changing how a naval fight plays.

A deterministic router lays physical conduits between compartments along the compartment adjacency graph. It routes two paths where the geometry allows, avoids magazines and the waterline, and breaks ties by cell index so the same design always routes the same way. Routes and default power priorities are auto-set values, each behind a named property test that runs in CI on every pull request (the auto-set rule is in The editor). X-ray view draws each network with its own colour, line pattern and label, with a legend, and badges doubled routes.

Every consumer has a priority from 1 to 5, and when power runs short the Engineer sheds load from priority 5 upward, so priority 1 is shed last. Default priorities by category:

| Priority | Consumers |
| --- | --- |
| 1 | Steering gear, the Data network and its control consoles, bilge and damage-control pumps |
| 2 | Propulsion motors (on electric-drive hulls), fire-control radars and directors, point-defence guns and missiles, decoy launchers |
| 3 | Main gun mounts (training, elevation, hoists), missile launchers, torpedo tubes |
| 4 | Search radars, sonar, ESM, drone launch and recharge |
| 5 | Cranes, RHIB davits, mine rails, the camera mast, passenger-berth hotel load, and every other non-combat consumer |

Priority gate (headless trial battery, checked at the vertical slice and then in CI): a power-starved variant of the one-against-three fight, with generation cut to 60% of combat demand, runs on all 6 starter hulls with 20 seeds each. It passes when steering, pumps and point defence stay powered 100% of the time and the median score with default priorities is at least 90% of the best of 5 hand-tuned schemes. Fallback: the best-scoring hand-tuned scheme becomes the default table, applied by category.

Experts can pin a route, split a network into zones joined by breakers, or add explicit Data links. These network edits ship at the first public release with L3. A pinned route survives regeneration like a pinned node, and a pinned route that a design change breaks becomes a Ship's Note.

### Officers

Five officers run the ship by default. Each officer is a graph asset built from the same nodes players use, so forking one is ordinary graph editing.

| Officer | Default job | Deliberate limits |
| --- | --- | --- |
| Helm | Heading and speed by PID, station-keeping, evasion patterns | No terrain-aware pathing beyond the theatre route |
| Engineer | Power and fuel priority, load shedding | Fixed priority order, no prediction |
| Sensors | Fuses detections into tracks with a simple gate | No multi-hypothesis tracking |
| Gunnery | Ranks threats, computes ballistic lead, deconflicts arcs | One target per mount, fixed salvo rules |
| Damage Control | Runs pumps, isolates compartments, orders repairs | Repairs in a fixed priority list |

The limits are documented on purpose. An expert fork that beats the defaults is a skill the game rewards, not an exploit. PvP has two rulesets. Standard, the default for public lobbies, allows doctrine presets, sliders and the rules table but bans forked officers and graph edits, so every ship uses stock fire control. Open allows forks and graphs.

Stock officers carry the campaign's opening act and then ask for tuning:

- Every Act 1 operation is winnable with stock officers on Normal. Gate (headless CI, nightly): each Act 1 operation runs with its starter design, stock officers and the Helm officer following the briefing route, over 20 seeds, and must win at least 16 of them. Fallback: a failing operation drops its last-added hostile hull, one at a time, until it passes; the officers are not changed.
- Acts 2, 3 and 4 each include at least one operation that the defaults fail but presets, sliders and the rules table can pass. None requires graph edits or scripts. Gate (same harness): stock officers win fewer than 10 of 20 seeds, and a reference L0 to L2 setup committed in the repo wins at least 16 of 20. Fallback: if no operation in an act meets that, the act's hardest operation gains one hostile hull at a time until stock officers win fewer than 10 while the reference setup still wins at least 16.

The graph regenerates whenever the design changes. Generated nodes use text.secondary frames. A node the player edits becomes pinned and gets a pin glyph, and pinned nodes survive regeneration. If a pinned node loses its input (you removed the radar it read from), it becomes a Ship's Note instead of disappearing, ranked in the "a weapon, sensor or consumer cannot work" tier.

### The control ladder

Each layer falls back to the one below it, so a broken expert setup never leaves the ship dead in the water.

| Layer | What the player touches | Ships in |
| --- | --- | --- |
| L0 Doctrine | Presets per ship or mount: aggressive, cautious, silent running | Vertical slice (M1) |
| L1 Tune | Three to six named sliders per behaviour, full parameter set under an expander | Vertical slice (M1) |
| L2 Rules | An ordered when/then table, such as "when a contact closes inside 4 km, switch Gun 1 to it" | First public release |
| L3 Graph | The live graph, with current values printed on its edges; fork, pin and edit any node | First public release, with the node library below |
| L4 Script | Luau code in a node with typed ports | First update after the first public release, once the fuzzing criterion in Scripting passes |

Graph editing ships at the first public release because the second pillar forbids features that exist only in automatic mode, and the officers run on the graph anyway. Luau follows in the first update after it, once the sandbox and budget work has passed the fuzzing criterion below.

L2 rules compile into graph nodes, so every rule is also visible at L3. A ship holds at most 32 rules. Rules are evaluated every graph tick (30 Hz), top to bottom. A matching rule overrides the officer's output on the sink it names for as long as its condition holds; when two matching rules write the same sink, the higher row wins. The tick after a condition stops holding, the officer's output returns.

### Node library

Every stage of every officer is a node, and its inputs and outputs are typed ports. The library at the first public release:

| Group | Nodes | Readable | Writable |
| --- | --- | --- | --- |
| Sources | Own-ship state (pose, speed, heel, power, fuel, damage), part state per part, raw detections per sensor with their error, track list with uncertainty, ID rung and stance, theatre orders | Yes | No |
| Math and logic | Arithmetic, compare, select, latch, timer, counter, PID, low-pass and alpha-beta filters, vector maths | Yes | Yes |
| Officer stages | Track fusion, threat ranking, mount assignment, ballistic solver, proportional-navigation guidance, load shedder, damage-control planner, emission controller | Yes, including internal gains and per-tick results | Parameters; any stage can be replaced by a player subgraph |
| Sinks | Helm heading and speed, mount aim point and fire permission, missile launch and target, emission state per emitter, power priority per consumer, pump and bulkhead commands, manual stance and ID override per track | Yes | Yes |

A player can therefore replace one stage (for example, threat ranking) and keep the officer's other stages automatic.

Every automatic value carries an A chip. Overriding it swaps the chip for a hand glyph. The Bridge Board shows each officer's current action in one line, how many values you have overridden, a diff against the default, and one-click revert. Officers report in text with radio effects (squelch, static, band-pass) and are never voiced.

Why? works on any part: right-click and the game prints the chain of causes from graph state, such as "Gun 2 idle: solution needs +74 degrees, mount maximum is +70." Each link opens the node responsible. Automation that holds fire without a visible reason reads as a bug, so every hold and every shot carries a reason string.

### Scripting

L4 scripts are Luau, run through the mlua crate pinned at =0.12.2 ([mlua 0.12.2](https://crates.io/crates/mlua/0.12.2)) with features = ["luau"] only. Native code generation is off because the luau-jit feature is never enabled, so Luau runs in its bytecode interpreter. The send feature is added only if a VM has to move between threads. mlua 0.12.2 depends on mlua-sys 0.13.0, which builds Luau from luau0-src ^0.22.0. Cargo.lock fixes the resolved version (0.22.1+luau744 today), and a Luau upgrade is a deliberate lockfile change behind full CI.

Each vehicle gets its own VM, created with Lua::new_with(StdLib::ALL_SAFE ^ StdLib::OS, LuaOptions::default()). The os library is never loaded: Luau's sandbox keeps os.clock, os.time and os.date, which would leak timing and break reproducibility. The host then calls set_memory_limit(bytes), calls set_interrupt with the budget counter, and calls sandbox(true) last. sandbox(true) makes every library table and the globals read-only, so the host removes or replaces globals before that call, never after.

Limits per vehicle:

| Limit | Default | What happens when it trips |
| --- | --- | --- |
| Memory | 8 MB per vehicle VM (set_memory_limit) | The allocation raises a memory error; the script greys out |
| Interrupt budget | 5,000 interrupts per vehicle per graph tick, shared by all of that vehicle's script nodes | The interrupt callback returns Err and ends the running script for that tick; the script greys out |
| Wall-clock backstop | 2 ms per script per graph tick, about 8 times the expected cost | The script is disabled and that tick is marked not reproducible in the log |

Scripts run at the 30 Hz graph rate. A vehicle's script nodes run in the graph's topological order, ties broken by node id, and draw on one shared counter. The interrupt callback increments that counter and returns Err once it passes the budget, so the budget is a deterministic work count, not elapsed time. It fires only at calls and loop iterations, and mlua ignores GC-step interrupts, so every host function a script can call must be constant-time or chunked ([Luau sandbox](https://luau.org/sandbox)).

When a script throws, hits the memory limit or overruns the budget, every output it wrote that tick is discarded, it greys out, and the officer stage it replaced drives those sinks from that same tick. Script nodes later in the order that did not run that tick also fall back to their officer stages for that one tick and run normally on the next. A greyed-out script stays disabled for the rest of the sortie. The player re-arms it in dry dock or during a splash, where graphs are edited.

Scripts run only on the authoritative host. Clients receive their outputs, and replays record the server's output stream, so Luau never needs bit-identical results across platforms. Luau won over WebAssembly (heavier tooling for modders) and Rhai (slower, with less proven budget controls), and its Roblox lineage means many players have written it before.

Budget gate (fuzz-corpus benchmark, at the release candidate of the first update after the first public release): worst-case scripts from the fuzz corpus that use the full 5,000 interrupts must run at p99 0.25 ms or less per graph tick, and 4 scripted vehicles must stay inside the 1.0 ms control share of the tick split in Performance budgets (4 x 0.25 ms per 30 Hz graph tick is 0.5 ms per 60 Hz tick). It is measured in two places: a CI job on the free GitHub-hosted Windows x64 runner (4 CPUs), and the owner's Apple Silicon Mac under the min-spec throttle profile (3 worker threads). Each runs the benchmark 5 times, the gate takes the median of the five p99 values per host, and the slower host decides. Community reports from the in-game benchmark scene confirm the result on real minimum-spec machines after release. The shipped example scripts must each use under 1 MB. Fallback: if p99 is over 0.25 ms, the interrupt budget becomes floor(5,000 x 0.25 / measured p99 ms), rounded down to a multiple of 500. That number is fixed for the release and identical on both platforms, so the budget stays a deterministic count. If any shipped example script needs more than 8 MB, the memory limit rises to 16 MB.

Fuzzing criterion for shipping L4. The host API is fuzzed with cargo-fuzz (libFuzzer) and structure-aware generation of Luau programs against the host API, through two targets (generated host-call sequences and raw Luau source). Every fuzz VM is set up exactly as in production, in the order above. L4 ships when all of these hold:

- at least 30 consecutive days of nightly fuzzing with no failing night, and at least 1,000 CPU-hours in total inside that window;
- zero sandbox escapes (any file, network, os or debug access, or any write outside the VM);
- zero host crashes or panics;
- zero ticks where a script overran its interrupt budget and was stopped only by the wall-clock backstop;
- at least 90% line coverage of the host API crate, measured with llvm-cov;
- every published Luau and mlua security advisory reproduced as a passing regression test.

The schedule fits the free hosted runners. The standard Linux x64 runner for public repositories has 4 CPUs and 16 GB, and a job may run for up to 6 hours ([GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners), [Actions limits](https://docs.github.com/en/actions/reference/limits)). The nightly fuzz workflow runs each of the two targets as a 5-hour job with libFuzzer -fork=4. That is 2 x 5 x 4 = 40 CPU-hours a night, so 1,000 CPU-hours accumulates in 25 nights, inside the 30-day window. A night that does not run because of a runner outage adds no hours and does not reset the clock. The gate is checked at the release candidate of the first update after the first public release.

Fallback if the criterion is not met at that release candidate: the update ships L4 with scripts enabled only for the local player's own vehicles, in single-player and in sandbox lobbies where the host is the only player with scripts. No host runs a guest's script, which removes the escape risk. Guest scripts unlock automatically in the first update after the criterion passes.

Upgrades and security fixes. Any mlua or Luau upgrade restarts the 30-day clock and must first pass a burst run: each target as two parallel 6-hour jobs with -fork=4 (96 CPU-hours, about 6 hours of wall time) with zero findings. Guest scripts that were already unlocked stay unlocked while the clock reruns. The security window: a hotfix ships within 72 hours of an upstream sandbox-escape fix. If no upstream fix exists yet, or the burst run on the fixed version finds anything, a hotfix that disables guest script hosting (the fallback mode above) ships within 72 hours instead and stays until the criterion passes again. Other security fixes ship within 14 days.

### When things break

Conduits are physical, so a hit that destroys cells can cut them. A part then falls back in order: the redundant path (within one tick), then its local buffer, then local control. A powered part's local battery holds 30 s at its rated draw, and a fuel consumer's day tank holds 60 s at its rated flow; both refill from the network while it is connected. A part that loses only its Data link skips the buffer step. Local control uses the part's own sensor with three times the normal error, and the HUD says so.

A Damage Control team restores a cut conduit at 50% capacity with a jury-rig job that takes 20 seconds, scaled by surviving berths as set in Structure and crew. Teams work down a repair list you can reorder. A jury-rigged conduit stays at 50% until it is repaired at the tender.

## Sensors and fire control

Radar tracking works the moment a radar and a weapon share a ship. The pipeline has five stages, each writing plain data the next one reads, so a newcomer sees finished tracks and an expert can open any stage and see the raw numbers.

| Stage | What it computes | Rate | Newcomer sees | Expert can open |
| --- | --- | --- | --- | --- |
| Signature | Radar cross-section by aspect, projected above-water and submerged area by aspect, emissions, acoustic noise, infrared, wake | Per tick | Nothing | Signature per aspect in the editor |
| Detection | Per sensor-target pair: horizon cull, range against reference, clutter, jamming, visibility, sonar layer | 2 to 10 Hz per sensor, staggered | Nothing | Raw detections with their error |
| Track | Fused, filtered track with uncertainty, a quality grade and an ID rung | 10 Hz | A symbol, a quality grade, an ID rung and one line of reason | Filter gains, fusion weights, the candidate list |
| Engagement | Which mount fires at which track, salvo size, hold reasons | 10 Hz | "Engaging" or "Holding: reason" | The full doctrine table, per mount |
| Guidance | Gun lead solutions, missile and torpedo steering | 30 Hz | Shell splashes and missile trails | Solution error, seeker state |

### Detection rates and budget

Default detection rates per sensor: search and navigation radar 5 Hz (designers may set 2 to 10 Hz per part), fire-control radar and missile seekers 10 Hz, ESM, sonar, visual lookouts and camera masts 2 Hz. Rates are staggered: a sensor with period p ticks (60 / rate) evaluates on the ticks where (tick + hash(sensor id)) mod p is 0. Rotating search and navigation radars turn at 24 rpm (2.5 s per rotation), the rate the plot's sweep draws at. Each evaluation checks only targets inside the sector swept since the last one, so every target gets one search-radar detection per rotation. Fire-control radars and seekers stare and update every evaluation.

The model is closed-form and costs a few floating-point operations per pair, so the sizing case of 300 sensor-carrying units (the reference battle's 41 hulls plus missiles, aircraft and drones, with headroom) at the 5 Hz default, about 450,000 pair checks a second before the spatial grid culls most of them, fits in the sensor budget. The sizing case counts every pair at 5 Hz and ignores the sector filter, so it is a ceiling.

The 8.0 ms tick (wall time per 60 Hz tick across the 3-core worker pool; lower-rate systems averaged per tick) splits as: rigid bodies and collider rebuilds 1.5 ms; buoyancy and waves 1.25 ms; projectiles 1.0 ms; damage, flooding, splitting and compartment recompute 1.0 ms; sensors (detection, tracks and engagement) 1.0 ms; control (graph VM, officers, guidance, Luau) 1.0 ms; enemy AI and steering 0.5 ms; replication and snapshot encoding 0.5 ms; reserve 0.25 ms.

- Gate: a 10-minute reference battle with per-subsystem tracing spans. Total p99 must be 8.0 ms or less, and the sensor share 1.0 ms or less at p99 under the 300-unit sizing case at the 5 Hz default. It runs at Milestone 0 spike 2, at each milestone and on each release tag, on the owner's Apple Silicon Mac under the min-spec throttle profile (thread count capped to the 3-worker pool). A nightly job on the free GitHub-hosted Windows and macOS runners runs the same battle headless and fails on a rise of more than 10% in any subsystem's work units against the trailing 7-night median; work units are deterministic, so hosted-runner noise does not affect them. The in-game benchmark scene reports the same per-subsystem p99 split, and community reports verify the Windows minimum spec.
- Fallback, applied in order until the sensor share fits, triggered by the Mac gate or by the median of at least 5 community reports from 6-core Windows machines exceeding 1.0 ms: (1) the default search-radar detection rate drops from 5 Hz to 3 Hz, while fire-control radars and seekers keep their rates; (2) the spatial-grid cell size is set to the fastest of 2, 5, 10 and 20 km measured in the spike; (3) pairs where both the sensor and the target are on hulls in kinematic motion (rung 2 of the degradation ladder in Performance budgets) run at 2 Hz, the bottom of the 2 to 10 Hz range. If the total fails, that degradation ladder applies.

### Detection model

Radar steps:

1. Cull by radar horizon using the 4/3 effective Earth radius. The horizon between two points is the sum of both: d = 4.12 x (sqrt(h\_a) + sqrt(h\_t)) km, with antenna height h\_a and target height h\_t in metres (the target's highest point, its altitude, or the emitter's height for ESM) ([radar horizon](https://en.wikipedia.org/wiki/Radar_horizon)).
2. Compare range against a designer-set detection range R\_ref at a reference cross-section sigma\_ref = 100 m², scaled by the fourth root of the target's effective cross-section over the reference: R = R\_ref x (sigma\_eff(theta) / sigma\_ref)^(1/4). Sixteen times the cross-section doubles detection range.
3. Aspect: sigma(theta) is read from the Signature stage's 36 bins of 10 degrees, where each bin's projected hull area is perpendicular to the line of sight, times material factors. This is the same table the editor shows per aspect. Sea clutter: sigma\_eff = sigma(theta) / (1 + C). C is 0 in calm water, 0.5 at Beaufort 4 and 2.0 at Beaufort 7, at full strength for a target top at 5 m or lower, falling linearly to 0 at 15 m. Emission control: a radar in Silent, or in Burst between sweeps, contributes no detections and has no ESM signature.
4. Apply self-screening jamming: the jammer-to-signal ratio grows with the square of range, so every jammed radar has a burn-through range where it starts seeing the target again ([electronic attack notes](https://usafa-ece.github.io/ece-495-ew/block02/L17_ElectromagneticAttack_Reading.html)). A jammer is rated by its burn-through range R\_bt0 against a radar with R\_ref 40 km and a 100 m² target. Against any radar, R\_bt = R\_bt0 x (R\_ref / 40 km)² x sqrt(sigma\_eff / sigma\_ref), and the jammed radar detects the target only inside min(R, R\_bt). Starting R\_bt0 is 10 km, so a jamming 150 t craft (about 150 m²) is seen by a frigate-class search radar at 12.2 km instead of 44 km. A radiating jammer is an emitter: every ESM receiver within the radar horizon gets its bearing, and Silent switches it off.

Detection is deterministic: a pair inside its final range is detected on every evaluation, and only the measurement error is random. Starting measurement errors (1 sigma): search radar 50 m in range and 0.5 degrees in bearing; fire-control radar 5 m and 0.05 degrees; missile seeker 2 m and 0.1 degrees; ESM 2 degrees bearing only; passive sonar 3 degrees bearing only; active sonar 20 m and 1 degree; visual and camera 10% of range and 0.2 degrees.

Starting reference ranges against 100 m²: navigation and search radar on a small mast 20 km, frigate-class search radar 40 km, fire-control radar 25 km, missile seeker 12 km. Reference cross-sections: sea-skimming missile 0.1 m², drone 0.5 m², aircraft 5 m². Hulls use their computed Signature, about 150 m² for a 150 t craft and about 7,000 m² for an 1,800 t hull.

Designers tune reference ranges and jammer ratings, never exponents or measurement errors. The fourth-root and square laws make range very insensitive to raw power, which makes exponents a tuning trap. Passive ESM detects emitters at long range with bearing only, out to min(radar horizon from the emitter's height, 2.0 x that emitter's R\_ref), and a radar you switch on lights you up to every ESM receiver in range.

Visual lookouts and camera masts detect out to R\_vis = 10 km x sqrt(A(theta) / 100 m²), where A(theta) is the Signature stage's projected above-water area for that aspect; a camera mast doubles R\_vis. Both are capped at the radar-horizon cull above and at the weather visibility V from the replicated weather state, the same V the renderer fogs to (density 3.912/V), so the simulation never sees farther than the render shows. At night R\_vis is multiplied by 0.3 unless the target fired in the last 10 s, is burning, or shows lights; ship lights burn only in Active emission control. A smoke screen blocks every visual and camera line of sight that crosses it. A headless CI invariant samples 10^5 pairs per weather preset and fails on any visual or camera detection beyond V or the horizon.

- Gate (trial battery, every PR): the simulated first-detection range for the three Sight gauge reference targets, by radar and by visual lookout, must be within 10% of the gauge's prediction. Headless CI test: in Beaufort 7, a frigate-class search radar (R\_ref 40 km) must detect the 0.1 m² sea-skimming missile at 5 km or more, which the defaults give at about 5.4 km. Jamming: a headless test checks first detection of a jamming target within 2% of the closed-form R\_bt, and a jamming 150 t craft must still be detectable by a frigate-class search radar at 8 km or more, the 57 to 76 mm gun cap, so a jammer never denies medium-gun range against the smallest jamming hull.
- Fallback: if the gauge drifts, the gauge adopts the simulation formula. If missile detection falls under 5 km, the Beaufort 7 clutter coefficient is solved in closed form from the range law so that the reference missile is detected at exactly 5 km (C of about 3.1 or less with no aspect loss). If the jamming floor fails, R\_bt0 rises until the 150 t craft is detected at 8 km.

Sensor noise comes from a counter-based hash RNG keyed on (seed, tick, sensor id, target id). The simulation never calls Rust's std sin, cos, tan, exp, ln, powf, powi, atan2 or the other functions whose precision the Rust docs call non-deterministic ([f64 docs](https://doc.rust-lang.org/std/primitive.f64.html)). A clippy disallowed\_methods list enforces this. It calls the libm crate, pinned at =0.2.16 with default features. The IEEE-exact std operations stay allowed: +, -, \*, /, sqrt, mul\_add, abs, floor, ceil, round and trunc.

libm's transcendental functions are plain f64 arithmetic, and Rust never fuses a\*b+c into FMA on its own, so they compute the same bits on aarch64 and x86\_64. Its architecture-specific paths are hardware sqrt and fma on x86\_64 (fma picked at runtime by CPU detection), and sqrt, fma and rint on aarch64. Each of these is a correctly rounded IEEE operation, so hardware and software give the same result; cbrt is the only function that calls fma internally. No in-house approximations, unless libm lacks a function; any such approximation is written in the same exact operations.

NaN sign and payload bits may differ between architectures. State hashes and serialization therefore canonicalize NaN, and a debug assertion flags any NaN reaching sim state.

Upgrading libm is a replay-breaking change. A golden-vector test hashes every libm function the simulation uses over a fixed set of 10^5 inputs and compares the result to a committed hash. It runs per platform in the regular CI battery, and across platforms in the nightly hash comparison on the hosted runners, which stays an early warning as described in Determinism.

### Sonar

The first public release ships hull sonar, passive and active, on a layer-and-shadow model. It detects torpedoes, mines and surface noise, which torpedoes, mines and Silent emission control depend on. AI-only enemy submarines arrive in Act 3 with depth charges and use the same model; player-built submarines are not in scope. Convergence zones are never simulated: they need deep open water and appear at tens of kilometres, which the archipelago's straits and shoals seldom offer.

- Passive: R = R\_ref x sqrt(N / N\_ref), the one-way spherical-spreading law. N is the target's acoustic noise from the Signature stage: current propulsion power divided by 2 MW, doubled while the propeller runs above its cavitation speed (default 20 kn). N\_ref = 1.0. A running torpedo has N = 1.0; mines are silent. Starting hull-sonar R\_ref is 8 km. Own speed above 15 kn halves passive range, and above 25 kn the hull sonar hears nothing.
- Active: the two-way radar law, R = R\_ref x (A\_sub(theta) / 100 m²)^(1/4), where A\_sub is the Signature stage's projected submerged area. Starting active R\_ref is 4 km, which finds a moored mine (about 1 m²) at about 1.3 km. Active sonar is an emitter: Silent turns it off, Burst pings only during bursts, Active pings continuously, and any passive sonar hears the pings out to 2.0 x the active R\_ref.
- Layer and shadow: each theatre region's data sets a layer depth from 30 to 80 m (default 50 m); water shallower than the layer depth has no layer. Hull sonars sit above the layer. When sensor and target are on opposite sides of it, range is multiplied by 0.3, and a target farther than 5 km on the other side is in the shadow zone and undetectable.
- Gate (trial battery, 200 seeded runs each): at 12 kn own speed, passive hull sonar detects an inbound reference torpedo at least 60 s before impact in 95% of runs, and at 10 kn active sonar detects a moored mine 800 m or more ahead in 95% of runs. Fallback: torpedo noise N rises in steps of 50%, and active R\_ref in steps of 25%, until each passes.

### Tracks and identification

Each track runs an alpha-beta filter with an uncertainty that grows between updates. The map draws that uncertainty in the shape the sensors produce: a wedge for a bearing-only contact, a small circle under a fire-control lock. The drawn shape is the 95% probability contour (2.45 sigma for a 2D ellipse). Multiple sensors fuse by inverse-variance weighting. Own-side units (drones, co-op hulls) share detections over the datalink at the 10 Hz track rate, fused into one picture per side.

- Gains follow the Benedict-Bordner relation beta = alpha² / (2 - alpha). Alpha is 0.5 for fire-control radar and seekers (beta 0.167), 0.4 for search radar (beta 0.094) and 0.3 for ESM, sonar and visual (beta 0.053), applied at each sensor update.
- Between updates, uncertainty grows as r(t) = sqrt(r0² + (sigma\_v t)² + (0.5 sigma\_a t²)²), where sigma\_v is the filter's velocity uncertainty and sigma\_a is the assumed manoeuvre acceleration: 0.3 m/s² for surface and subsurface, 20 m/s² for aircraft and 50 m/s² for missiles.
- Bearing-only ranging gets process noise from the first build; Sea Power had to add it to its target motion analysis in a 2026 update after passive ranging proved too accurate ([Sea Power 0.7.0 report](https://simulationdaily.com/news/sea-power-update-070-new-campaign/)). Bearing-only tracks use sigma\_a = 0.5 m/s² with the per-sensor bearing errors above, and the range uncertainty of a single-sensor bearing-only track never drops below 10% of the estimated range.
- Association uses one gate, matching the Sensors officer's documented limit (no multi-hypothesis tracking): a detection joins the nearest track whose 99% contour (Mahalanobis distance 3.03 sigma) contains it, otherwise it starts a new track. A track drops after 120 s without an update for surface and subsurface tracks, or 20 s for air and weapon tracks; the plot keeps a last-known mark for 10 minutes.
- Quality grade, shown on the symbol and used by doctrine: Lock (updated by a fire-control radar or seeker within the last 1 s), Fine (95% radius of 250 m or less), Coarse (95% radius of 2 km or less), Bearing (bearing-only, or 95% radius over 2 km).
- Gate (headless CI, 500 seeded runs): (1) for active tracks, the true position lies inside the drawn shape in 90% to 98% of samples; (2) for bearing-only, against a 15 kn target at 20 km with one own-ship two-leg manoeuvre over 10 minutes, the 1-sigma range uncertainty stays at 10% or more and the truth lies inside the drawn shape in at least 90% of runs. Association gate (reference battle, nightly): track swaps under 1% of track updates, and no truth object inside detection range holds two live tracks for more than 10 s.
- Fallback: if truth falls inside the drawn shape in under 90% of samples, every sigma\_a for that domain is multiplied by 1.5 until it passes. If it falls inside in over 98% (uncertainty drawn too large), sigma\_a is multiplied by 0.75. The gains stay fixed. If swaps exceed 1%, the association gate shrinks to 2.5 sigma; if duplicates persist, it grows to 3.5 sigma.

Identification climbs a five-rung ladder (Unknown, Domain, Type, Class, Exact), with stance kept separate (Friend, Neutral, Suspect, Hostile). Command: Modern Operations uses the same split at scale ([CMO data types](https://commandlua.github.io/assets/DataTypes.html)). Each track holds a candidate list: every entry in the theatre's order of battle, civilian traffic list and own-side roster consistent with the evidence so far. The rung is the finest level at which all remaining candidates agree. Evidence that narrows the list:

- Domain: any detection that measures altitude or depth, or an emitter or acoustic match.
- Type: an ESM emitter match (every Combine radar model is in the emitter library from the first operation), a radar cross-section and speed consistent with one type for 30 s, or a passive acoustic signature match.
- Class: an ESM match to an emitter fitted to only one class, or a visual or camera sighting inside 50% of R\_vis.
- Exact: a visual or camera sighting inside 25% of R\_vis, a completed Photograph action, or a briefing that names the hull.

A track card shows the shrinking candidate list and says what moved it up a rung: an emitter match, a visual sighting, an acoustic signature. Symbols and terms are original to Black Pennant, not borrowed naval symbology; Interface and visual design sets the symbol and colour rules.

### Engagement doctrine

Every weapon and target-category pair has a doctrine entry with editor-set defaults, inherited from fleet to ship to mount:

- Anti-ship missiles fire at Hostile tracks identified to Type or better and graded Coarse or better, two per salvo. They hold ("Holding: Neutral inside seeker basket") while a Neutral track lies inside the seeker's search area around the predicted target position.
- Guns fire at Hostile tracks graded Fine or better inside effective range, and at Suspect tracks only when they close inside self-defence range.
- Surface-to-air missiles fire at Hostile air tracks graded Fine or better inside effective range, one per target, with a second after a miss when time allows.
- Point defence (guns and missiles, graded Lock) engages inbound weapons automatically, whatever launched them. A weapon track is inbound when its predicted closest approach to any own-side hull is 500 m or less within the next 60 s. Chaff and flares launch when an inbound missile is inside 6 km, one salvo per missile, with a 10 s reload; towed decoys stream while a torpedo track is inbound.
- Nothing auto-engages a Neutral or Friend track, ever.

Default effective range per mount equals the Punch gauge's effective range: the range at which the predicted single-round hit probability against a 40 m reference hull under a fire-control lock is 10% or more in Beaufort 4. It is clamped to these starting caps: 20 to 40 mm autocannon 3 km; 57 to 76 mm 8 km; 100 to 127 mm 12 km; 130 to 155 mm heavy guns 18 km. Anti-ship missiles: 80% of motor range. Surface-to-air: 80% of kinematic range against the target domain. Point defence: guns 2 km, missiles 8 km. Self-defence range is one value per ship, inherited by every gun, defaulting to 4 km, which matches the rule example in The control ladder. A mount whose effective range is shorter uses that range instead.

- Gate (trial battery, vertical slice): across the 6 starter hulls, the first-salvo hit rate at the default effective range must be 10% or more, and default doctrine's rounds fired per kill in the one-against-three fight must be within 30% of a hand-tuned doctrine.
- Fallback: the class caps are removed and effective range becomes purely the solver-derived 10% range per mount. If rounds per kill are still more than 30% worse, the hit-probability criterion rises to 15%.

Experts override any entry per mount, write rules or scripts, or take a mount and aim it by hand.

### Stance

Stance is separate from identification and is set by these rules, in order:

1. Own-side and co-op hulls, drones and missiles are Friend over the datalink and can never be engaged, even by hand. Friendly fire from ballistic spread still exists.
2. A track becomes Hostile when it fires on your side, when its emitter matches a Combine radar, when the mission briefing marks its class hostile, or, in PvP, when the lobby puts it on the opposing side.
3. Defectors, decoy convoys and civilian traffic start Neutral when the briefing declares them; everything else starts Suspect.
4. The player or a graph can override stance per track; the track card always shows the reason for the current stance.

### Emission control

The Sensors officer manages emissions by default with an L1 slider: Silent, Burst or Active. Every radar, jammer and active sonar follows it. Silent keeps every emitter off and works from ESM, passive sonar, visual lookouts and drones; the one exception is point-defence fire-control radars, which radiate while an inbound weapon is inside 8 km, shown on the Bridge Board. Burst (the default) stays passive until a threat is inside the trigger range or a weapon is inbound, radiates in short sweeps to refresh tracks, and holds a fire-control lock only while own weapons are in flight or point defence is engaging. Active radiates continuously. The Bridge Board shows which emitters are radiating and warns when an enemy ESM receiver has probably detected you. Briefings recommend a setting when an operation's ghost conditions forbid emissions.

- Burst defaults: start radiating when any Suspect or Hostile track from a passive source (ESM, sonar, drone, visual) is inside 20 km, or when a weapon is inbound. The editor suggests a trigger range per design. Each burst lasts 3 antenna rotations (7.5 s). While the trigger holds, bursts repeat every 30 s, and every 10 s while a track with a weapon assigned is graded below Lock.
- ESM warning, computed from player knowledge, never ground truth: the Bridge Board warns "Enemy ESM has probably detected you" when any own emitter radiated in the last 60 s and any Suspect or Hostile track, or briefing-declared ESM site, lies within min(radar horizon from that emitter's height to an assumed 20 m mast, 2.0 x the emitter's reference range).
- Gate (scripted trial, vertical slice): an ESM duel against one Hostile picket. In Burst, the enemy's first ESM detection must come at least 2 times later than in Active, and the gunnery hit rate must be within 20% of Active. The warning must have fired before or within 1 s of the true first enemy detection in 95% of 100 seeded runs.
- Fallback: if the hit rate is more than 20% worse, the repeat interval shortens to 15 s. If detection is not delayed 2 times, bursts drop to 1 rotation. If the warning comes late in more than 5% of runs, the assumed enemy mast rises to 30 m.

Experts can replace the emission controller stage with their own graph, for example to radiate only when a drone has lost contact.

### Guidance

Gun solutions iterate predicted target position and shell time of flight until they converge ([naval fire control fundamentals](https://man.fas.org/dod-101/navy/docs/fun/part19.htm)). A solution converges when the predicted impact point moves less than 0.25 m (half a cell) between iterations, with a cap of 6 iterations. An unconverged solution fires on the last iterate and logs the reason "Solution unconverged". The solver uses the same drag model as the projectile simulation and accounts for turret slew time. It aims at the track estimate, not the true position, so a poor track shows up as visible shell dispersion and the player learns what track quality means by watching misses.

Missiles steer by proportional navigation, set per missile class: N = 3 for anti-ship missiles (surface targets with low acceleration) and N = 4 for surface-to-air and point-defence missiles. Players can tune N at L1 and L3. Each missile costs a few vector operations per tick.

- Midcourse: the missile receives updates at 1 Hz over the datalink while the launching ship, or any own-side unit sharing the track, holds the target track. The datalink has no horizon limit, a deliberate simplification. If updates stop, the missile flies to the last predicted position.
- Terminal: the seeker switches on when the range to the predicted target position falls below its detection range against the track's estimated cross-section plus the track's 95% radius. From then an active radar seeker is an emitter under the ESM rule (2.0 x 12 km, capped by the horizon). It acquires a return inside its field of view of plus or minus 30 degrees and inside its own detection range under the radar law, with clutter and jamming. Seekers reject own-side returns over the datalink but not Neutral ones.
- Decoys: when a decoy enters the seeker's resolution cell (inside the field of view and within 300 m of the locked return), one draw from the counter-based RNG keyed on (seed, tick, missile id, decoy id) moves the lock to the decoy with probability sigma\_decoy / (sigma\_decoy + sigma\_target), and the seeker keeps the result. A chaff cloud is 1,000 m² at bloom, falling linearly to 0 over 60 s, which seduces about 87% of locks on a 150 t craft and about 13% on an 1,800 t hull. Flares use the same rule against infrared seekers: a hull's infrared value is its current propulsion power in MW, and a flare is worth 20 MW for 5 s. Towed decoys use it against torpedo seekers, radiating noise N = 4.0.
- Torpedoes run straight to the predicted position, or with wire updates at 1 Hz on wire-guided designs, then home with an active acoustic seeker under the active sonar law and N = 3. Depth charges and mines are unguided.
- Gate (headless CI): (1) in the reference battle and trial battery, at least 99.9% of gun solutions converge within 6 iterations, and the mean aim-point error against a 50-iteration solution is 0.25 m or less; (2) over 200 seeded runs, the median surface-to-air miss distance against an aircraft weaving at 3 g is 5 m or less, and the median anti-ship miss against a 30 kn hull turning hard is inside the target's bounding box; (3) over 200 seeded runs, a reference anti-ship missile launched at a Coarse track of a 30 kn hull at 30 km acquires in at least 90% of runs without decoys, and one chaff salvo seduces it in 60% to 90% of runs against a 150 t craft and 10% to 40% against an 1,800 t hull.
- Fallback: if convergence is under 99.9%, the cap rises to 10. If the surface-to-air miss median is over 5 m, air-target classes go to N = 5. If the anti-ship median misses the box, anti-ship goes to N = 4. If acquisition is under 90%, the seeker field of view widens to plus or minus 45 degrees. If a chaff band fails, the chaff bloom cross-section moves in 25% steps toward the band; if no value meets both bands, the large-hull band wins.

In multiplayer, the server holds the truth and sends each client only its own side's tracks, plus the true pose of objects inside an own-side unit's visual range, which the 3D view needs to draw them. A modified client cannot reveal what its sensors have not detected.

## Combat and damage model

Ships die the way ships do: holed below the waterline, flooded, listing until they capsize or break in two. Hits resolve per 0.5 m cell, while flooding and buoyancy run per compartment, which keeps a 90 m hull of about 100,000 cells affordable to simulate. Enemy capitals up to 150 m (at most 4 hulls over 90 m per operation, counted in the reference-battle budget) scale roughly with length cubed, about 460,000 cells at 150 m. Damage is the same physical model at every difficulty; no setting scales penetration or warhead tables.

The sim treats wedge, corner and inverse-corner cells as full cells for occupancy, traversal, flooding and buoyancy. The one exception is armour obliquity, which uses the shaped cell's surface normal from its shape and rotation byte, so sloped armour works as it looks.

A vessel is disabled when no propulsion part works, or when it strikes its colours (Campaign). It is lost when its main piece capsizes (heel beyond 90 degrees for 10 s) or sinks (no cell of its top deck above the water surface).

### Weapons

Every weapon ships free with the game, and none is sold separately. Stormworks sells its weapons as a DLC, and negative reviews repeatedly call that a paywall that splits lobbies and the Workshop ([Search and Destroy on Steam](https://store.steampowered.com/app/1542360/Stormworks_Search_and_Destroy/)).

| Family | Examples | Parts at first release | Role |
| --- | --- | --- | --- |
| Guns | 20 to 40 mm autocannon; 57 to 127 mm medium guns; 130 to 155 mm heavy guns from Act 3 | 7 (20, 30, 40, 57, 76, 100 and 127 mm) | The main ship-killer at short and medium range |
| Missiles | Light and heavy anti-ship, surface-to-air, short-range point defence | 4 | Long reach, but they announce themselves to ESM and can be decoyed |
| Underwater | Torpedoes, mines; depth charges from Act 3 | 3 (light and heavy torpedo tubes, and the mine that mine rails lay) | Holes below the waterline, and the main threat to large hulls |
| Defence | Point-defence guns, chaff, flares, smoke, towed decoys | 5 (one of each) | Survive the swarm |
| Offboard | Launched drones; a small helicopter deck from Act 2 | 2 drones | Extra sensor geometry for a lone ship |

Heavy guns unlock in Act 3 under its 900 t cap. Mount weight and the tonnage cap gate them, with no separate hull-length rule. Depth charges arrive with the AI-only enemy submarines in Act 3, and if submarines slip, depth charges slip with them. The helicopter deck arrives with Act 2. It carries a stock helicopter flown by an officer, not one the player builds. At the first public release, drones are the only offboard sensors. Sandbox, skirmish and PvP offer every part in the build from first launch. Heavy guns, depth charges and the helicopter deck join the build when their act ships. Mine rails count among the hull and system parts, not the underwater weapons.

Offboard sensors exist because a single ship has a single viewpoint. A bearing-only contact needs a second sensor at an angle to fix its range, and a drone gives the lone player that geometry.

### Projectiles and armour

Projectiles are flat arrays integrated ballistically at the fixed 60 Hz tick. Integration is semi-implicit Euler with gravity (9.81 m/s²) and quadratic drag, a = -k |v| v, using one k per calibre band. Wind does not deflect projectiles. Projectile ids are assigned in spawn order, and spawns within a tick are ordered by (vessel id, mount id). The live cap is 2,000 projectiles. The degradation ladder in Performance budgets can drop it to 1,500 and then 1,000; above the cap, one autocannon projectile carries 2 rounds and applies its hit twice. A shell that reaches the water surface ends there and emits a splash event, so guns never travel underwater. Point-defence rounds self-destruct 4 s after firing.

Each tick, a projectile's segment from its old position to its new one is tested against vessel bounding boxes in a uniform hash grid of 128 m cells, so fast shells cannot tunnel. A segment that enters a box is transformed into the vessel's local grid and stepped cell by cell with an Amanatides-Woo traversal, which costs about two comparisons and one addition per cell crossed ([Amanatides and Woo](https://www.eecs.yorku.ca/~amana/research/grid.pdf)). Hits are sorted by (tick, projectile id) before they apply, so damage never depends on thread timing.

Cell damage is a bucket from 0 (intact) to 8 (destroyed), stored as a u8. Health is 1 - bucket / 8. This is the value the damage event stream replicates as cell index plus bucket. The four damage levels the interface draws map onto it: Intact is buckets 0 and 1, Damaged 2 to 4, Heavy 5 to 7, and Destroyed 8. A part's health is the mean health of its cells. A part works normally at Intact and Damaged, at 50% output (rate of fire, power, flow, sensor range) at Heavy, and not at all at Destroyed. Destroyed cells vanish from the sim. Flying debris is client-side VFX only.

Penetration follows a de Marre-style curve, where penetrable thickness grows with impact velocity to the 1.4 power: P(v) = P_1km x (v / v_1km)^1.4, where v_1km is the shell's velocity at 1 km. An armour cell resists its tier thickness divided by cos(obliquity), and beyond 70 degrees of obliquity the shell ricochets and is deleted. The table stores P_1km and the 1.4 exponent as inputs. A build script fits each band's drag coefficient k so that the ballistic impact velocity at the band's cap range gives the far-range value below. That value is the fit target, and CI fails if the fitted ballistics miss it by more than 5%.

| Band | P at 1 km (input) | P at cap range (fit target) | Cap range | Fuse delay | Path damage K | HE value B | HE radius r |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 20 to 40 mm | 20 mm | 8 mm at 3 km | 3 km | 1 cell | 2 | 4 | 1 cell |
| 57 to 76 mm | 45 mm | 25 mm at 8 km | 8 km | 2 cells | 3 | 6 | 2 cells |
| 100 to 127 mm | 90 mm | 55 mm at 12 km | 12 km | 4 cells | 4 | 8 | 3 cells |
| 130 to 155 mm (heavy) | 150 mm | 100 mm at 18 km | 18 km | 6 cells | 4 | 8 | 5 cells |

With the four armour tiers (5, 15, 40 and 80 mm), this means autocannon defeat the 5 and 15 mm tiers up close, 57 to 76 mm guns defeat 40 mm up close, 100 to 127 mm guns defeat 80 mm up close, and heavy guns defeat 80 mm at every range. The cap ranges match the class caps for effective range in Sensors and fire control.

A shell steps through cells while it has penetration left:

1. Air cells cost nothing. Each unarmoured solid cell costs 2 mm of remaining penetration. Each armour cell costs its effective thickness.
2. Each solid cell the shell crosses takes the band's K buckets of path damage.
3. The fuse fires after the band's delay, counted in solid cells after the first solid cell hit. If the shell meets an armour cell whose effective thickness exceeds the penetration left, it stops and bursts at that cell's outer face.
4. A non-penetrating hit that reaches at least 50% of the needed thickness spalls: the cell directly behind the armour takes 1 bucket.
5. High explosive damages every cell within r cells of the burst (Euclidean distance between cell centres, d of r or less) by ceil(B x (1 - d / (r + 1))) buckets. For the 100 to 127 mm band that gives 8, 6, 4 and 2 buckets at distances 0 to 3. An intact armour cell on the straight line from the burst to a target cell blocks HE to that cell, which is found with the same traversal. Armour cells themselves take floor(B / 2) at the burst centre, scaled by the same falloff.

Bucket results are integers. Any scaled value rounds to the nearest integer, with a minimum of 1.

Missiles and underwater weapons use the same cell rules with these starting values. The starting ranges for missiles are kinematic ranges. Sensors and fire control sets their effective ranges at 80% of these figures.

| Weapon | Speed | Range | Fuse | B | r |
| --- | --- | --- | --- | --- | --- |
| Light anti-ship missile | 270 m/s | 30 km | Penetrates 40 mm, then a 2-cell delay | 8 | 4 cells |
| Heavy anti-ship missile | 290 m/s | 70 km | Penetrates 40 mm, then a 4-cell delay | 8 | 6 cells |
| Surface-to-air missile | 600 m/s | 25 km | Proximity, 8 m, against air targets; the 20 to 40 mm band's values against hulls | 4 | 1 cell |
| Point-defence missile | 700 m/s | 10 km | Proximity, 5 m, against air targets; the 20 to 40 mm band's values against hulls | 4 | 1 cell |
| Light torpedo | 40 kn | 8 km | Contact | 8 | 4 cells |
| Heavy torpedo | 45 kn | 15 km | Contact | 8 | 7 cells |
| Mine | Laid | 10 m influence radius | Influence | 8 | 6 cells |
| Depth charge (Act 3) | Sinks at 3 m/s | Set depth of 10 to 150 m | Depth | 8 | 6 cells |

Missiles, drones and aircraft are small bodies with a bounding sphere and hit points: a sea-skimming missile has a 0.5 m sphere and 1 hit point, a drone 1 m and 2, and an aircraft 5 m and 6. A projectile segment that passes within the sphere removes 1 hit point. Point-defence gun rounds also count a hit when they pass within 1.5 m of the sphere's surface. A proximity missile that bursts within its lethal radius removes all of a target's hit points.

Defence objects are sim objects, identical on every quality tier, and seekers see them through the rules in Sensors and fire control:

- Chaff is a radar cloud of 1,000 m² cross-section. It blooms over 3 s, lasts 60 s and drifts with the wind.
- A flare is an infrared decoy that lasts 8 s.
- A towed decoy streams 200 m astern and presents twice the ship's radar cross-section to radar seekers.
- Smoke is a cylinder 60 m in radius and 30 m tall. It reaches full size in 10 s, lasts 120 s and drifts with the wind. It blocks optical and infrared line of sight, including the camera mast, but not radar.

Gates (headless CI calibration on the free hosted runners, from the vertical slice; medians and rates over 200 seeded runs):

| Gate | Pass band | Automatic fallback |
| --- | --- | --- |
| A 76 mm gun against the 22 m fast attack craft at 4 km: hits to disable or lose it | Median 8 to 20 | Scale that band's K and B linearly until the median sits at the band's midpoint (14). Radii and penetration stay. Then re-check the penetration pattern against the tiers. |
| A 30 mm autocannon against the same craft at 1.5 km | Median 40 to 120 | As above, to 80 |
| A 127 mm gun against the 90 m destroyer at 10 km | Median 25 to 60 | As above, to 42 |
| One heavy torpedo amidships on the 22 m fast attack craft | Lost in at least 90% of runs | Raise the heavy torpedo's r by one cell at a time until it passes |
| Heavy torpedoes amidships on the 90 m destroyer | One hit: survives in at least 60% of runs. Two hits: survives in 20% or fewer. | Change r by one cell toward the failing side. If both conditions cannot hold, the one-hit condition wins. |
| One point-defence gun on the 40 m missile corvette against one sea-skimming missile (250 m/s, 5 m altitude, Beaufort 4) | Killed before 500 m in 60% to 85% of runs | Scale the 1.5 m proximity distance linearly, between 0.5 and 3 m, toward 72% |
| A 2-round chaff salvo fired when an incoming anti-ship missile's seeker turns on | Seduces it in 40% to 70% of runs | Scale chaff cross-section linearly toward 55% |
| A towed decoy alone against the same missile | Seduces it in 30% to 60% of runs | Scale the decoy's cross-section factor linearly toward 45% |
| A 5-charge depth-charge pattern dropped on a sonar fix with 50 m error against the Act 3 reference submarine (from Act 3) | Disables it in 30% to 60% of patterns | Scale the depth charge's r by whole cells toward 45% |

Each fallback runs as a script that rewrites the table in a commit, and the gates then run again.

### Flooding, stability and fire

Water moves through every opening by the orifice equation Q = Cd x A x sqrt(2 g dh). Q is in m³/s, A is the opening area in m², g = 9.81 m/s², and Cd = 0.6. That coefficient is the industry-standard discharge coefficient that the international damaged-ship flooding benchmark recommends for all openings ([Ruponen et al. 2022](https://doi.org/10.1016/j.apor.2022.103153)). It is a default kept in the flooding table.

A hull breach is an opening between the sea and a compartment, and each destroyed outer cell face counts as 0.25 m². A destroyed bulkhead or deck cell, or an open door or hatch, is an opening between two compartments. Cells leak only when destroyed (bucket 8).

dh is the net head across the opening: the water surface height on one side, measured above the opening's centre and floored at zero, minus the same on the other side. The sea side uses the wave height sampled under that cell. Flow runs from the higher head to the lower, and it stops when both sides are below the opening.

Each tick moves Q x dt (dt = 1/60 s), clamped to the volume that would level the two surfaces that tick. That volume is dh x A_i x A_j / (A_i + A_j) between compartments with free-surface areas A_i and A_j, or dh x A_i against the sea. The clamp stops overshoot and oscillation. Compartments are treated as vented, with no air cushion. Flows are computed per vessel in parallel, and openings apply in ascending compartment id order.

For scale, a single 0.25 m² breach 2 m below the waterline takes in 0.6 x 0.25 x sqrt(2 x 9.81 x 2), about 0.94 m³/s, and 1 m down it takes about 0.66 m³/s. Gameplay pacing is set by a separate flood_rate_scale (default 1.0), never by changing Cd. sqrt is IEEE-exact in Rust, so the formula needs no libm call.

Water fills the compartment and adds weight low or high depending on where it is. Each tick, a compartment's water surface is a level plane in world space. Its height is found by 16 bisection steps against the compartment's cells at the current heel and trim, and the water's weight acts at the centroid of the filled volume. As the hull heels, loose water shifts toward the low side, so the free-surface loss of stability comes out of the model.

Closed bulkheads contain the water. Pumps remove it:

- A small pump moves 0.05 m³/s and a large pump 0.15 m³/s, in the same unit as Q.
- A pump drains its own compartment, or one directly adjacent compartment that Damage Control selects, deepest water first.
- Each compartment's water level replicates at 2 Hz as a u8 fill fraction, which the renderer draws as that compartment's own water.

Damage Control patches breaches:

- A team seals one 0.25 m² breach face per 10 s base time, scaled by the berth factor in Structure and crew.
- A face can be patched only while its compartment is under 50% flooded.
- A patch has 1 bucket of health, so any further damage to the cell reopens it.

Every compartment below the waterline has a 0.1 m² flood valve that obeys the same orifice law. Opening it floods the compartment, to fight a fire or to counter-flood against list. By default only the player opens these valves, through the pump and bulkhead commands. Magazines are fed from the fire main instead and fill completely in 10 s.

Buoyancy is computed from the real hull. Buoyancy blocks are 2 m cubes (4 by 4 by 4 cells), each with the solid and watertight fill fraction of its cells. The water height is sampled once per 2 m column per tick and shared by every block above that column, so heel, list and capsize come out of the physics rather than a stability meter. The editor's GZ curve predicts the same behaviour. The trial battery compares the predicted equilibrium heel, and the heel recovery against the GZ curve, with the simulation, and it fails on more than 10% drift, the same rule as every gauge.

Flooding gates (headless CI on the free hosted runners, 200 seeded runs, from the vertical slice):

| Gate | Pass band | Automatic fallback |
| --- | --- | --- |
| The 22 m fast attack craft with one 0.25 m² breach 1 m below the waterline in its largest compartment, with default Damage Control | Afloat, with that compartment under 50% flooded, in at least 90% of runs | Scale the 10 s patch base time down linearly until it passes, to no less than 5 s. Beyond that, lower flood_rate_scale in steps of 0.1. |
| The Resilience gauge's one-compartment and two-compartment cases, with each case opened to the sea and pumps off, on all 6 starter hulls | Equilibrium heel and trim within 10% of the gauge | The gauge adopts the simulation's method |
| Buoyancy and waves in the reference battle | Within their 1.25 ms per tick share | Hulls more than 2 km from every player hull use 4 m blocks and 4 m columns |

Fire starts in compartments that take hits near fuel or ammunition, spreads to neighbours over time, and burns down the systems inside. All draws use the counter-based RNG, keyed by sortie seed, tick, vessel id, compartment id and draw kind.

- Ignition: a penetrating or HE hit with any affected cell within 2 cells of a fuel cell ignites its compartment with 15% chance, or 25% if a cell is within 2 cells of an ammunition cell.
- Spread: each burning compartment ignites each adjacent compartment, including those across decks, with 1% chance per second (mean 100 s). The chance halves across a closed bulkhead.
- Intensity: a fire starts at 100 and does not grow. A compartment more than 50% flooded cannot burn.
- Burn-down: every part in a burning compartment loses 1% health per second. Structural cells do not burn.
- Cook-off: a magazine in a burning compartment cooks off after 120 s unless it is flooded. A cook-off destroys every cell within r = clamp(4 + floor(cbrt(stored ammunition mass in kg)), 4, 24) cells of the magazine's centroid. A 76 mm magazine of 1,800 kg gives 16 cells (8 m), and a large 127 mm magazine reaches the 24-cell (12 m) cap.
- Suppression: each Damage Control team assigned to a fire sprays it down by 2 points of intensity per second, so one team puts out a fresh fire in about 50 s. A flooded magazine's fire goes out immediately, and its ammunition is lost until rearmed.

By default, Damage Control floods a burning magazine once 60 s of its cook-off delay remain and the fire is not projected to be out in time. A magazine fire that nobody fights is how a ship dies in one hit.

Fire gates (headless CI, 200 seeded runs):

| Gate | Pass band | Automatic fallback |
| --- | --- | --- |
| An unfought fire in the magazine compartment of the 22 m fast attack craft | The magazine cooks off at 120 s, and the craft is lost | Scale the cook-off delay to the middle of the band |
| An unfought fire in the compartment next to the magazine | The craft is lost within 2 to 5 minutes in at least 80% of runs. The defaults give about 83%: 120 s of cook-off plus a spread time with a 100 s mean lands inside the window with probability 1 - e^-1.8. | Scale the cook-off delay linearly to the middle of the band |
| A single-compartment fire with default Damage Control | Out within 90 s in at least 90% of runs | Scale the suppression rate linearly to the middle of the band |
| Fires per sortie, measured over 200 seeded headless Act 1 sorties with stock officers on Normal (the same harness as the Act 1 winnability check) | Median 1 to 4 | Scale both ignition chances linearly to the middle of the band (2.5) |

### Structure and crew

When cells are destroyed, the vessel is marked dirty. At the end of the tick, a flood fill seeded from the removed cells' neighbours runs over face-adjacent solid cells. It finds any part no longer connected to the vessel's root cell, which is the keel cell nearest the design's centre of mass, fixed at spawn. That part splits off as its own floating body.

If the root cell is destroyed, the piece with the most cells keeps the vessel id, with ties going to the lowest cell index. New pieces get body ids in ascending order of their lowest cell index. Compartments are recomputed by a flood fill over air cells bounded by watertight cells. Merged compartments sum their water.

Pieces smaller than 64 cells leave the sim and become client debris. Larger pieces float as wrecks with their own compartments and buoyancy. A wreck despawns when it is fully submerged more than 20 m down, or after 600 s, and a sortie keeps at most 16 wrecks, removing the oldest first. Salvage is recorded when a vessel is lost (Campaign sets the amount), so removing wrecks from the physics world loses nothing.

Splitting and compartment recomputes run under a per-tick work budget of 50,000 cells visited. The budget is shared across vessels in ascending vessel id order, and leftover work carries to the next tick in that order. It is one constant in sim-core, the same on every platform, and changed only by a commit, never at runtime. Heavy fire cannot spike the frame, and the result never depends on CPU speed.

The tick shares this section spends are 1.0 ms for projectiles and 1.0 ms for damage, flooding, splitting and compartment recompute (the split is in Performance budgets).

Splitting gate (Milestone 0 spike 2, then nightly):

- Where it runs: headless on the free hosted Windows x86_64 runner and on the owner's Apple Silicon Mac under the minimum-spec throttle profile. Each machine runs the benchmark 5 times and takes the median, and the slower of the two medians gates. Benchmark-scene reports from community minimum-spec machines re-check the gate before each release.
- Cost: the measured cost per cell visited times 50,000 must fit in 0.4 ms of the 1.0 ms damage share.
- Backlog: in a heavy-fire soak with 2,000 projectiles concentrated on the 90 m stress hull, the backlog must clear within 30 ticks (0.5 s), so a split shows within half a second. With the same fire on a 150 m enemy capital, it must clear within 60 ticks.
- Fallback if the cost is over 0.4 ms: the budget becomes floor(0.4 ms / measured ns per cell), rounded down to a multiple of 5,000, and is committed for the release.
- Fallback if the backlog misses: the acceptable latency becomes 60 ticks (120 for the 150 m hull), and vessels that any player-side sensor tracks are processed before untracked vessels.
- A community report that exceeds the backlog limit triggers the backlog fallback in the next release.

There is no walking crew. Crew exists as capacity, which removes interior navigation and character netcode from scope and keeps damage about systems, not people.

- Teams: Damage Control has floor(berths / 4) teams, at least 1 and at most 6. Each team runs one job at a time, so teams equal concurrent jobs.
- Berth factor: job time scales by original berths / surviving berths. Berths in compartments that are destroyed, burning or more than 50% flooded do not count. With no surviving berths, no job can start.
- Starter examples: the 22 m fast attack craft has 8 berths (2 teams), and the 90 m destroyer has 24 berths (6 teams).

Base job times, before the berth factor:

| Job | Base time |
| --- | --- |
| Jury-rig a cut conduit at 50% capacity (from When things break) | 20 s |
| Patch a breach face | 10 s per face |
| Fight a fire | 2 points of intensity per second |
| Repair a part | 30 s per job. The job restores 25 percentage points of health, up to a cap of 75% (bucket 2). Destroyed parts and destroyed cells are repaired only at the tender, for Salvage. |

The default repair list, which the player can reorder, is:

1. Fires in or next to a magazine.
2. Breaches below the waterline, deepest first.
3. Other fires.
4. Cut conduits feeding priority 1 and 2 consumers.
5. Other cut conduits.
6. Part repairs, highest power priority first.

Ties go to the lowest compartment index.

Berth gate (trial battery, vertical slice, 100 seeds): adding 50% more berths to a starter hull, with the weight cost included, must raise its survival rate in the one-against-three fight by at least 10 percentage points, and removing half the berths must lower it by at least 10 points. Fallback: if either change is under 10 points, berths per team drop to 3. If either is over 30 points (berths dominate the design), berths per team rise to 5.

## Campaign

The campaign is a persistent theatre in which one combat hull per player, supported by drones and a tender, starves and outfights an occupying navy of about 300 ships on Normal. It runs four acts of 8 to 12 hours each, over 40 hours in total, and about a quarter of the content is authored. The first public release (GitHub, version 0.x) ships Act 1. Each later act is a free update that is mostly data on top of systems that already exist.

### Fiction

The Meridian Combine, a corporate navy, has occupied the Saltreach archipelago: 600 by 400 km of islands, straits and shoals in four regions. The player commands Task Unit LANTERN, a deniable special detachment with one combat hull and a tender, SEXTANT, that moves between hidden coves. Each cove is a full save point and a dry dock. These names are final.

The premise justifies the core rule. LANTERN is outnumbered in every operation, so a good design and a good plan beat tonnage. The detachment wins by cutting supply lanes, blinding radar networks and picking fights it can finish, not by trading broadsides with a battle fleet.

### The theatre

The theatre is a graph of 48 nodes joined by lanes, shown on a chart table. Time moves only when the player commits to a transit, a refit or a wait. The theatre then ticks in one-hour steps, animated and pausable, until the next decision or interrupt. A continuous real-time theatre was rejected because it is harder to keep in sync in co-op and harder to test headless.

A sortie starts at the theatre hour at which it is committed, in that region's weather at that hour (sea state, wind, cloud, visibility V and rain, from the seeded, replicated weather state). That fixes the sortie's sun and fog (see Graphics and rendering) and its time-of-day band for doctrine tallies, from the sun elevation that Graphics and rendering's hour table gives that theatre hour: night below 3°, dusk from 3° to under 25°, day from 25°. The briefing's forecast is that weather.

The Combine runs on logistics that the player can see and cut:

- Shipyards produce tonnage, depots store fuel and munitions, convoys carry them along lanes, and forward bases spend supply to launch task groups. A task group is 5 hulls whose combined displacement is at least the operation tonnage multiple (4x by default, see Core loop) times the act's hull cap: 600 t in Act 1, 7,200 t in Act 4.
- Each region has a Supply value from 0 to 100: 100 times the supply delivered to the region over the last 4 theatre days, divided by its demand over the same window. Patrol density is (25% + 0.75 x Supply/100) of the region's base patrol count, and Combine repair and refit speeds scale by the same factor. A successful convoy raid or a minefield cuts its lane for 10 theatre days. A depot strike empties the depot, which counts as a cut on every lane the depot feeds for the same 10 days. Cutting a lane thins patrols within two to four theatre days, and the chart animates the change.
- Regional Alert is a real number from 0 to 5, shown and compared rounded down. It drops by one for every 48 quiet hours. In the region where the event happens, a kill that a surviving Combine unit observes or reports adds 1.0. An unobserved kill adds 0.5 when the loss is noticed 12 theatre hours later. A sighting (any Combine unit holds LANTERN as Suspect or Hostile) adds 0.5 per contact, at most 1.0 per operation. Evidence (a wreck found, an intercepted emission, a detected RHIB) adds 0.25 each, at most 1.0 per operation. At Alert 3, escorts, air patrols and decoy convoys appear. At Alert 5, a hunter-killer group sorties against SEXTANT, and the briefing warns of it at least 24 hours ahead. The group is one task group of the act's top enemy tier, led by an ace not yet beaten in the act, if one remains. If it reaches SEXTANT's cove, the next operation is a surviving-a-hunter-group operation that cannot be declined.
- Each act has an operation clock until a Combine objective completes: 40 theatre days on Normal, set by the clock slider. Success pushes it back: 1 theatre day per template operation, 2 for a depot strike or radar decapitation, 2 per ace duel won, and 3 per story operation. If the clock runs out, the act is lost and the next act starts in the harder retreat configuration rather than ending the campaign. If the Act 4 clock runs out, Act 4 restarts in retreat configuration, and Open Theatre opens only after the Act 4 capstone. A normal act starts every region at Alert 0 with 3 coves. A retreat act starts every region at Alert 2 with 2 coves (the act script names which cove closes) and keeps the difficulty's clock length.

XCOM 2 shows that a visible doom clock makes every choice matter, and its reviewers show it also stresses players, so clock speed is a difficulty setting ([XCOM 2](https://en.wikipedia.org/wiki/XCOM_2)).

### The enemy

The Combine fields about 300 persistent hull records at Normal difficulty, adjustable from 200 to 450. Each record tracks fuel, ammunition, damage, veterancy and refits, so a crippled task group comes back weakened. Every enemy class is built in the player's own editor and validated by Fit Check in CI: about 20 classes across five tiers at the first public release, about 60 by 1.0. Enemy capitals may reach 150 m against the player's 90 m limit, with at most 4 hulls over 90 m in any operation, counted in the reference-battle budget. Ultimate Admiral: Dreadnoughts switched new campaigns to pre-made AI designs in 2024 after years of tuning its auto-designer, so Black Pennant never generates enemy designs at runtime.

The enemy AI works on three levels:

1. The Admiral, a utility planner, replans every six theatre hours.
2. Each task-group commander keeps a belief map of where LANTERN probably is, fed by sightings, radar, radio emissions, ESM and missing convoys, and searches its peaks. Decoys and emission control fool a real model rather than a scripted cheat, and players can view the overlay after a mission.
3. Each unit runs a behaviour tree on top of the same sensor and fire-control pipeline the player's ship uses, at the difficulty's accuracy and reaction time.

Ships hold formation by flocking and keep doctrine-set engagement ranges. No Rust crate does naval pathing, and Godot navigation is excluded because sim-systems has no Godot dependency, so pathing is our own code on two small crates, in three layers:

1. Route. A* on a coarse grid of the sea map with 50 m cells, using the [pathfinding](https://crates.io/crates/pathfinding) crate (4.16, astar): 8-connected, octile heuristic, u32 costs of 10 per straight step and 14 per diagonal, plus 20 for a cell next to land, ties broken by cell index. The grid stores water depth per cell. Each hull class has a clearance mask that blocks cells within half its beam plus 10 m of land, or shallower than its draft plus 2 m.
2. Smoothing. Line-of-sight string-pulling over the clearance mask, then a Dubins turn at the hull's turning radius for its planned speed at each remaining corner, using [dubins\_paths](https://crates.io/crates/dubins_paths) 3.3 (f64 API, default-features = false, features = ["libm"]). A corner whose arc leaves clear water keeps its unsmoothed waypoint. Final approaches to a pose (formation slot, station, berth) use DubinsPath::shortest\_from.
3. Steering. Turning-circle-aware pure pursuit with a lookahead of at least one turning radius. Flocking and collision avoidance adjust heading and speed only, never the path.

Reeds-Shepp (reversing) paths are not used; backing off is a scripted manoeuvre. Replanning shares a budget of 2,000 A* node expansions per 60 Hz tick across all hulls, in a first-in, first-out queue. The Milestone 0 exit test: 40 ships cross a 100 km map with islands and a harbour, every ship reaches its harbour berth, no ship enters a blocked cell, paths are bitwise identical across runs and between macOS and Windows, and 95% of replans finish within 1 s of simulated time (if not, the budget rises to 4,000). If the harbour leg fails, cells drop to 25 m and the test re-runs. If it fails again, hybrid A* (x, y and heading with motion primitives and Dubins analytic expansion) is written by hand on the same grid for confined water only: harbours, straits and the cutting-out template.

Adaptation uses about 30 authored doctrine cards. The game tallies how you win with exponential decay: each kill adds 1, times the adaptation cadence slider, to four tallies (kill method, engagement-range band, time-of-day band, emission state at the time of the kill), and each tally has a half-life of 120 theatre hours. When a tally reaches 10, the matching counter goes on the doctrine bulletin with an arrival date 72 theatre hours ahead: rely on missiles and point defence thickens, raid at night and radar pickets appear. In that window the player can spend Intel at the chart table to delay the card by 72 hours (2 Intel) or cancel it (5 Intel). Each region holds at most two active cards. Metal Gear Solid V showed that adaptation reads well when it is visible and can be countered ([MGSV dynamic difficulty](https://www.gamerguides.com/metal-gear-solid-v-the-phantom-pain/guide/the-basics-6/gameplay-9/dynamic-difficulty)).

Seven named Combine captains (the aces) bring signature ships, doctrines and radio callsigns, with two ace arcs in each of Acts 1 to 3 and one in Act 4. Their lines are text played with radio effects (squelch, static, band-pass); no ace or officer is voiced. Their returns are authored and fixed in the script. No enemy ever remembers individual player actions, and the project commissions no patent opinion: Warner Bros. holds an active patent on nemesis-style enemy memory, US10926179B2, adjusted expiry 2036 ([Google Patents](https://patents.google.com/patent/US10926179B2/en)).

### Missions

Eleven parametric templates are filled from theatre state: convoy raid, depot strike, radar decapitation, agent extraction, minelaying, prize capture by boarding, defector escort, feint, photo reconnaissance, surviving a hunter group, and cutting a ship out of harbour. Each act adds four story operations unlocked by theatre conditions (for example, Region 2 Supply below 40), one or two ace duels, and a capstone that unlocks when the act's four story operations are done. Sorties take 15 to 30 minutes.

Each operation has two or three optional ghost conditions, such as no radar emissions, no losses, or capturing the ace. They pay out Intel or blueprints and reward a specialised design over one universal ship. MechWarrior 5's procedural missions went stale when difficulty only meant more and heavier enemies, so operations combine five separate parts (objective, environment, task-force makeup, active doctrine, complication) ([MechWarrior 5 review](https://www.vice.com/en/article/mechwarrior-5-mercenaries-review)). Each template can be offered at most 3 times per act and never twice in a row. Story operations, ace duels, forced operations and the capstone do not count against the cap.

Every Act 1 operation is winnable with stock officers on Normal, checked by headless CI runs. Acts 2 to 4 each include at least one operation that stock officers fail on Normal but that presets, sliders and the rules table can pass; none requires graph edits or scripts.

### Special-operations actions

The special-ops templates need actions that work without walking crew. Each is a part plus a timed station action that the player starts and the enemy can interrupt:

| Action | Part | Rules |
| --- | --- | --- |
| Board and capture | Boarding kit: RHIB davit plus boarding-team berths | Target disabled (no propulsion, or colours struck), within 200 m, own speed under 5 kn. Capture takes 90 s and aborts if either ship takes fire or the target regains propulsion. A captured prize sails home under its own Helm officer or is stripped on the spot for Salvage and blueprints. |
| Insert or extract | RHIB davit | Hold under 3 kn within 100 m of a shore marker for 60 s. Enemy patrols that detect the RHIB raise Alert and can sink it. |
| Carry passengers | Passenger berths (adds weight; a constraint tag sets the count) | Passengers in a berth compartment that floods or burns are lost, and losing more than the briefing allows fails the operation. |
| Recover survivors | RHIB davit or a recovery net | Same hold rule at a survivor marker. Captured crew becomes Intel. |
| Lay mines | Mine rails | Draw a line on the plot table. One mine drops every 50 m along it while the ship holds within 5° of the line's course at 6 kn or more. Each mine has a 10 m influence radius. A field laid in an operation stays on its lane in the theatre and cuts it. |
| Photograph | Camera mast | A trainable mount with a 10° field of view and 6 km range. Hold the target inside the field of view, within range and in visual line of sight (the radar-horizon formula, using the target's highest point) for 20 s. The camera's range never exceeds the weather visibility V that the renderer fogs to (density 3.912/V). |

How the eleven templates use them: convoy raid and depot strike use weapons only; radar decapitation uses weapons or an insertion; agent extraction uses extraction and passengers; minelaying uses mine rails; prize capture and cutting out a ship from harbour use boarding; defector escort uses passengers and stance rules (the defector is Neutral until the briefing's rendezvous); feint and hunter-group survival use emission control and drones; photo reconnaissance uses the camera mast.

### The editor on every mission

Each briefing carries two to four constraint tags: a draft limit for a shallow harbour, a radar cross-section limit for a stealth insertion, a range, a passenger count, a required crane or mine rail. Fit Check runs a 30 s headless simulation in the briefing's forecast sea state (a full-power acceleration run, then a 180° turn) and scores range, speed, detection range against the known threats, and each tag as pass or fail. Range and detection come from the analytic gauges.

The Quartermaster is a bounded tool, not an auto-designer. It never changes the hull shell. For each failing tag it swaps kits from a curated list tied to that tag (a shallow-draft propulsion kit for a draft limit, low-signature mast kits for a cross-section limit), checks each candidate with Fit Check on the background worker pool, and offers the cheapest legal result as a diff of every changed part. Each candidate has a 300 ms wall-time limit, and the whole search has 5 s across the 3 workers, about 50 candidates. When nothing in the lists fits, it says "No legal refit found" and names the blocking tag. It never returns an illegal refit. The vertical slice scores it against hand-designed answers to a set of 20 briefings, at least 3 of them built to have no legal refit (gates below).

Refits cost Salvage and theatre time: two hours for a module swap, one hour per 10 t of structural change. Added parts cost their full price. Removed parts go to SEXTANT's parts store and can be refitted later for free; they never sell for Salvage.

Early operations double as editor lessons. Each one unlocks a tier of parts, the way From the Depths sorts blocks by complexity level, and asks the player to fix or extend a stock ship. There are 5 tiers: tier 1 at the start, tiers 2 and 3 within Act 1, tier 4 in Act 2 and tier 5 in Act 3; Act 4 adds parts to existing tiers. The first public release has about 60 parts: 7 guns (20 to 127 mm), 4 missiles, 3 underwater weapons, 5 defence systems, 2 drones, 8 sensors and about 30 hull and system parts, plus about 30 kits. Campaign unlocks gate only the campaign: sandbox, skirmish and PvP have every part from first launch, and PvP limits come from lobby rulesets.

### Economy

| Resource | Comes from | Spent on |
| --- | --- | --- |
| Salvage (S) | Wrecks and prizes | Parts, refits, repairs, rebuilds, ammunition and fuel shortfalls |
| Intel | 1 per ghost condition met, 2 per successful photo reconnaissance, 1 per prize crew or per 10 survivors recovered | Delaying a doctrine card 72 h (2) or cancelling it (5), revealing a region's convoys for 4 theatre days (1), revealing an ace's location (3) |
| Blueprints | Captured and boarded enemy parts | Permanent unlocks of those parts, within tiers already open |
| Fuel and ammunition | Raided depots and convoys, which refill SEXTANT's stock at no Salvage cost | Loaded physically into tanks and magazines from SEXTANT's stock; any shortfall is bought with Salvage |

MechWarrior 5's economy broke around hour 20 because payouts outran costs and damaged mechs resold near full price. Black Pennant keeps money tight: repairs and ammunition cost real Salvage, and damaged hulls have no resale value. Crew veterancy caps at +15%, so the design stays the main lever.

Starting values: a fully fitted 150 t Act 1 hull costs about 1,000 S to build, and part costs scale with mass and tier from that anchor. A wreck pays 20% of the sunk hull's build cost. A prize brought home pays 60%, or stripping it on the spot pays 40% plus blueprints for its parts the player has not unlocked. Repairs cost 50% of the damaged parts' cost. A full ammunition reload is valued at about 5% of the hull's build cost, the rate at which shortfalls are bought. The target median Act 1 ratio of operation income to spend (repairs, ammunition, refits) is 1.15 on Normal.

Each combat hull has a tonnage cap per act: 150 t, 400 t, 900 t and 1,800 t. In co-op every player's hull gets the same cap. The first act's cap keeps early designs small and readable. A single player never commands more than one combat hull, which keeps the lone-unit fantasy and avoids building a flotilla command interface. Each act offers new starter hulls (see Scope per act); sandbox and skirmish offer all six from the start.

### Failure and saving

A lost hull keeps its design but loses its fitted parts, and rebuilding costs 60% of its build cost in Salvage plus 48 theatre hours. If SEXTANT sinks, the detachment loses 30% of its Salvage, fuel and ammunition (Intel and blueprints are kept), the region returns to full Combine control (Supply 100, its cut lanes restored), and SEXTANT refits at the nearest cove in another region, which takes 72 theatre hours.

Full saves happen at coves and between operations. Quitting mid-sortie writes a suspend save from the latest 60 s operation checkpoint into one slot, and loading it deletes it. Ironman uses the same suspend slot, keeps one rolling full save and makes hull loss permanent: a lost hull cannot be rebuilt. An Ironman campaign ends when no player hull remains. In co-op Ironman, a player whose hull is lost keeps the chart-table vote and spectates the remaining sorties. Outside Ironman there is no campaign game over.

### Co-op

Up to two players at the first public release and four by 1.0. Each player runs their own hull alone; there are no shared hulls. Co-op scales the challenge with enemy blueprint variants (heavier armour tiers, extra escorts, better loadouts) and one extra task group per added player, so each added player brings the operation multiples with them. The co-op reference battle is 4 player hulls against a 25-hull base operation plus 3 task groups of 5: 40 hostile hulls and 44 full-physics hulls in all, the same hostile count as the reference battle. Its tick gate and the base-shrink fallback are in Performance budgets. If the host reached rung 2 or later of the degradation ladder (Performance budgets) in the previous operation, the next operation replaces its added task groups with quality upgrades (veterancy, loadouts, armour tiers). Co-op never adds a hidden hit-point multiplier, which would contradict the physical damage model.

The host's machine writes the campaign save, and every member keeps a copy, so a group is never locked to one player's PC. Joining a campaign happens between operations. A player who drops mid-operation can rejoin the same slot within 5 minutes, and their hull's officers keep running it meanwhile; after 5 minutes the officers keep it to the end of the operation, and the slot reopens at the next one.

Session rules for co-op:

- Theatre decisions. Any player proposes a transit, refit or wait on the chart table. The proposal is visible to everyone and commits when all players accept or the host confirms.
- Resources. Salvage, Intel, blueprints, fuel and ammunition are one shared pool on SEXTANT. A refit that would overdraw the pool commits only under the same rule as theatre decisions.
- Refits. Each player refits their own hull in their own dry dock at the same time. The theatre time spent is the longest single refit, not the sum.
- Time compression. It engages only while every player holds it, drops to 1x the moment any player releases or a contact enters weapon range, and the host's budget caps the rate. Pause stays single-player only.
- Host loss. The host writes an operation checkpoint (a full keyframe snapshot plus theatre state) every 60 s and sends it to members. If the host drops, any member can re-host through the normal hosting flow (see Multiplayer), and the operation resumes from the last checkpoint. If the host quits mid-operation, the checkpoint becomes the suspend slot in every member's copy.
- Save copies. Every save carries a sequence number and a hash of the previous save. When members' copies disagree, the highest sequence wins, and a copy that forked from it is refused with an explanation. Loading a suspend slot advances the sequence, which consumes it in every copy at the next sync. This exists to catch accidents between friends, not cheating.

None of the save and checkpoint data is hidden from co-op members, because co-op partners are trusted. PvP uses different rules (see Multiplayer).

### Difficulty

Six sliders set difficulty, with presets on top. There is no damage slider: damage is the same physical model at every difficulty.

| Slider | Range | Easy | Normal | Hard |
| --- | --- | --- | --- | --- |
| AI accuracy (aim-error multiplier; 1.0 is the player's stock officers, higher misses more) | 0.6 to 2.0 | 1.5 | 1.0 | 0.75 |
| AI reaction time | 4.0 s to 0.8 s | 3.0 s | 2.0 s | 1.2 s |
| Adaptation cadence (multiplies doctrine tally increments) | 0.25x to 2.0x | 0.5x | 1.0x | 1.5x |
| Hull count (persistent Combine records) | 200 to 450 | 220 | 300 | 380 |
| Economy (multiplies Salvage income) | 0.7x to 1.5x | 1.25x | 1.0x | 0.85x |
| Operation clock | 28 to 60 days | 50 days | 40 days | 32 days |

Custom is any other combination. Hull count locks at campaign start because it sizes the theatre. The other sliders can change between operations, except in Ironman, where all six lock at campaign start. Ironman is a separate toggle that works with any preset.

### Scope per act

Acts 2 to 4 are mostly data, but not entirely. The table separates new code from new content so slips are visible early.

| Act | Hull cap | Starter hulls added | New code | New content |
| --- | --- | --- | --- | --- |
| 1 (first public release) | 150 t | 15 m patrol boat, 22 m fast attack craft, 26 m low-signature insertion craft | Everything in the first public release build, including hull sonar (passive and active) on the layer-and-shadow model | About 20 enemy classes, 11 templates, 4 story operations, 2 ace arcs, capstone |
| 2 | 400 t | 40 m missile corvette | Enemy strike aircraft behaviour; player helicopter deck; part tier 4 | About 12 classes, 4 story operations, 2 ace arcs, capstone |
| 3 | 900 t | 60 m corvette with mine rails and boarding kit | AI-only enemy submarines and depth charges; heavy guns (130 to 155 mm, gated by mount weight and the tonnage cap); part tier 5 | About 14 classes, 4 story operations, 2 ace arcs, capstone |
| 4 | 1,800 t | 90 m destroyer | Open Theatre post-game | About 14 classes, 4 story operations, 1 ace arc, final capstone |

Sonar never simulates convergence zones. Open Theatre continues the same save after the Act 4 capstone with no operation clock: all four regions, template operations only, the Combine rebuilding from its shipyards, the 1,800 t cap, and a template repeat cap counted per 40 theatre days.

The protected scope is four acts without submarines. Under the roadmap rule for a missed milestone gate (one extension of 25%, then cuts by pillar priority), submarines are the first cut in Act 3 and Open Theatre the first cut in Act 4; a cut item moves to a free update after 1.0. If submarines are cut, depth charges go with them, Act 3 ships with surface and air threats, and the campaign still meets the 40-hour target.

### Tuning gates

Campaign bots are headless policies (aggressive, careful, single-method, mixed) that play the theatre layer in full and resolve each sortie with the abstract-combat model. They run nightly on free GitHub-hosted runners, sharded so no job passes the 6-hour job limit. Each week 20 sampled sorties replay in full physics. Bot gates count only while abstract and full-physics outcomes agree on at least 85% of those sorties (same win or loss, Salvage within 15%, Alert change within 0.5); below that, the gates run on 20 full-physics Act 1 runs per policy until agreement returns.

Playtest gates use the owner plus voluntary testers of GitHub pre-release builds. The game writes a local operation log (loop times, income, spend, ghost results) to the user data directory and never sends it anywhere; testers attach it to a GitHub issue form, which also asks the survey questions. A playtest gate needs at least 10 testers. With fewer at the milestone, the bot gates alone decide and the playtest gate re-runs at the next pre-release.

| Item | Pass | Automatic fallback |
| --- | --- | --- |
| Alert increments and retreat start | 200 Act 1 runs per policy on Normal: aggressive reaches Alert 5 in some region in at least 70% of runs, careful (emission control, unobserved kills) in 30% or fewer. A retreat act's completion rate is at least half a normal act's. | Kill increments scale until both policy bands pass. If retreat completion is under half, its starting Alert drops to 1. |
| Supply and lane cuts | 200 seeded runs: cutting a region's busiest lane lowers its patrol density by at least 15% within 4 theatre days in 90% of runs. | The Supply window drops to 3 days, then the patrol floor from 25% to 15%. |
| Operation clock | Mixed policy, Normal: finishes Act 1's capstone before the clock runs out in 60% to 85% of runs. | Template pushback moves in 0.5-day steps between 0.5 and 2 days. |
| Doctrine cards | 200 runs per policy: missiles only triggers the matching counter within 4 operations in at least 90% of runs; mixed triggers 2 or fewer cards per act on average; at least 50% of queued cards arrive while the policy holds the 2 Intel to delay them. | Threshold moves in steps of 1 between 6 and 16 until both policy bands pass. If the delay rate is under 50%, lead time rises to 96 h. |
| Intel costs | A policy meeting every ghost condition can cancel at least one card per act; a policy meeting none can delay at most one. | Cancel cost moves in steps of 1 between 3 and 8. |
| Repeat cap | 1,000 Act 1 bot runs: zero reach a state where no template can be offered. Playtest: under 25% of testers call operations repetitive at the end of Act 1. | A deadlock raises the cap to 4 for templates whose prerequisites that state met. A repetitive rate of 25% or more drops the cap to 2 for the most-repeated templates. |
| Mines and camera | Headless CI, 200 seeded crossings: an Act 1 enemy class crossing a 10-mine line at right angles hits at least one mine in 50% to 70% of crossings. Playtest: at least 70% complete Photograph on the first try in the photo-reconnaissance tutorial operation. | Spacing becomes (2 x 10 m + reference beam) / 0.6, rounded to 5 m. If photo completion is under 70%, range rises to 8 km and field of view to 15° (still capped by V). |
| Quartermaster score | Vertical slice: at least 18 of 20 right. Right means a refit that passes every tag at 120% or less of the hand answer's Salvage cost, or "No legal refit found" with the correct blocking tag on an unsolvable briefing. | Extend the failing tags' kit lists and re-score before the first public release. If still under 18 at its release candidate, the Quartermaster ships only for tags it gets at least 90% right; for others Ship's Notes names the tag and the manual fix. Any illegal refit disables it for that tag at once. |
| Fit Check time | Nightly, 20-briefing set plus the 90 m stress hull: p95 per candidate 300 ms or less and the Quartermaster total 5 s or less, on the GitHub-hosted Windows runner and on the owner's Mac under the minimum-spec throttle profile. | Screen candidates with the analytic gauges (held within 10% of the simulation by the trial battery) and simulate only the 3 cheapest that pass. If one candidate still exceeds 300 ms, the simulation drops to 15 s. |
| Economy | 200 Act 1 bot runs per policy on Normal: median income-to-spend ratio 1.05 to 1.25, top quartile under 1.5 at hour 8, bottom quartile over 0.8. Once shared operation logs cover at least 500 completed operations, they must also pass. | Wreck payout moves in steps of 2 points between 10% and 30% until the median is in band. If a quartile still fails, repair cost moves between 40% and 60%. |
| Stock officers | Headless CI: every Act 1 operation won by stock officers on Normal; each later act has at least one operation they fail and a slider or rules-table setup passes. | A failing Act 1 operation blocks the content merge until its composition is retuned. |

## Multiplayer

One machine runs the only full simulation, and every other machine sends inputs and draws what it receives. Single-player is the same program with the server running in-process over a loopback transport, so every campaign session exercises the multiplayer code path.

Where this section sets a gate, the test machines are the free GitHub-hosted runners (Windows x64, Linux x64, macOS arm64) and the owner's Apple Silicon Pro/Max Mac under the minimum-spec throttle profile (see Platform support). Bot matches on a hosted runner put the server and every bot client on that one runner, talking over loopback UDP through the link conditioner, because hosted runners accept no inbound connections from each other.

### Model

The server-authoritative model with state sync won over the two alternatives:

- Deterministic lockstep keeps bandwidth independent of object count, but it needs bit-identical simulation across compilers, operating systems and CPU architectures ([Gaffer on Games, deterministic lockstep](https://gafferongames.com/post/deterministic_lockstep/)). Nobody can verify that between Apple Silicon and x86 by hand, and lockstep puts the full world state on every client, which makes radar hacks trivial.
- Rollback re-simulates several frames per tick. Doing that for many-block ships and water is too expensive.

Ships are slow, so drawing remote vessels 100 to 150 ms in the past is hard to notice. Clients predict only presentation: the camera, turret slew under the player's hand, the aim reticle. Server corrections blend in through a decaying error offset rather than snapping. The offset decays with a 150 ms half-life; an error over 10 m (a split, a teleport after rejoin) snaps.

Interpolation delay:

- Default: fixed at 4 snapshot intervals at 30 Hz, which is 133 ms.
- Gate (Milestone 0 spike 3, then nightly bot matches on each hosted runner OS): at 150 ms latency, 5% loss and 30 ms jitter, the client runs out of snapshots and has to extrapolate in 1% or fewer of rendered frames.
- Fallback: jitter-adaptive mode. The delay is set to 2 intervals plus 2 times the p95 jitter over the last 5 s, clamped between 3 and 4.5 intervals (100 to 150 ms). The client moves between delays by changing its playback rate by up to 5%, never by jumping.

### What goes over the wire

Each vessel replicates as one or a few rigid bodies: a quantized pose (smallest-three quaternion, positions to about 2 mm), a rest flag, and joint and turret angles. Damage travels on a reliable, sequenced event stream: cell index plus damage bucket, and splits as cell sets plus new body ids. Logic runs only on the server, and only its visible outputs replicate. The wave seed and the weather state (sea-state code, wind, cloud cover, visibility V, rain) replicate as reliable events when they change, so every client's waves and render presets match the server's. Replication code is generated from one schema.

The estimated per-vessel cost is tens of bytes per snapshot instead of kilobytes. Stormworks needed a custom vehicle replication technique and its own compression to cut multiplayer bandwidth by 60 to 80% in 2022, and From the Depths' patch notes from 2021 to 2025 keep fixing one unsynced feature after another ([Stormworks news](https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid=573090&count=400&maxlength=0&feeds=steam_community_announcements), [From the Depths news](https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid=268650&count=500&maxlength=0&feeds=steam_community_announcements)). Generating sync from one schema is the defence against that pattern.

A priority accumulator fills each client's packet: objects that miss a packet keep their priority and go first next time ([Gaffer on Games, state synchronization](https://gafferongames.com/post/state_synchronization/)). The server sends each client only what that client's side has detected, so interest management and anti-wallhack are the same filter.

Projectiles never go into snapshots. A ballistic shell replicates once, as a spawn event (origin, velocity, tick, id, calibre) that the client integrates with the same drag model, followed by a reliable impact event. Guided missiles send state at 5 Hz and clients interpolate between updates. At 2,000 shells in flight, per-snapshot replication would cost hundreds of kilobytes a second; spawn events keep projectiles to a small share of the 256 kbps budget.

Bandwidth gate (Milestone 0 spike 3, re-run nightly in bot matches on each hosted runner OS):

- Per-vessel replication averages 20 bytes or less per update. At 41 hulls and 30 Hz that is about 197 kbps, which leaves about 59 kbps (23%) for projectile events.
- Projectile spawn and impact events are 25% or less of client bandwidth.
- Total per-client p95 is 256 kbps or less, measured in the spike's 2-player, 30-AI match and in a replay-driven reference battle with 41 hulls and 2,000 projectiles.
- Playable at 150 ms and 5% loss means no correction on the client's own hull larger than 0.5 m, and a manual-aim hit rate of at least 90% of the 0 ms baseline, measured with the aim bot described under Aiming.

If the total exceeds 256 kbps, these apply in order: (1) vessels other than the client's own replicate at 20 Hz, with the interpolation delay set to 3 x 50 ms = 150 ms; (2) projectile spawns batch into one event per salvo that carries per-round seeds.

At runtime each client has an adaptive cap between 128 and 768 kbps. It starts at 256 kbps, rises 10% every 10 s while that client's loss stays under 1%, and drops 25% when loss over a 2 s window exceeds 5%. The priority accumulator never sends more than the cap. Over the cap, lower-priority objects drop first: distant tracks to 1 Hz, then poses of untracked vessels.

Cross-platform check: hosted runners cannot join one live match, so each nightly run records the server output stream on Windows and decodes it on macOS, and the reverse. Decoded poses must match within quantization, and client-integrated shell positions must stay within 0.5 m of the server's at impact.

### Blueprints

Blueprints are content-addressed and cached on clients. The address is the BLAKE3 hash of the canonical RON bytes; the bytes travel and are stored zstd-compressed, the same encoding as share strings (see Sharing and mods). They stream in 16 KiB chunks on a low-priority reliable lane, limited to 25% of the client's cap while a match is live and uncapped in the lobby. The receiving end builds the ship on a worker thread and uploads it to Godot in slices of at most 0.5 ms per frame, inside the adapter's 2 ms budget, because Besiege and From the Depths both hit lag spikes and buffer limits loading large machines. The client cache keeps 512 MiB, least recently used first out. The server refuses any blueprint over 8 MiB compressed, then re-validates every blueprint it accepts: schema, part legality, ruleset limits, bounds and connectivity, and it recomputes all stats itself.

In PvP, opponents never receive your full blueprint. The server derives an exterior-only render blueprint (hull shell, visible superstructure, mounts) for the other team, and damage events reference exterior cells only. A full blueprint would reveal magazine positions and armour layout.

### Aiming and manual control

A manually aimed shot travels as a designation: a track id plus an offset, or a direction in the vessel's local frame, stamped with the client's render tick. The server fires at its current tick, not the client's. A track designation converts through the server's current track estimate, so it survives the delay. A direction-only shot is corrected by extrapolating the aimed-at target from the client's render tick to the server tick. There is no world-state rewind at launch.

Rewind trigger: every replay keeps a shot log (designation type, RTT, client render tick, target speed, hit or miss). The nightly aim bot fires direction-only shots with a fixed 250 ms human reaction model at targets moving 20 kn or more, and PvP playtest replays attached to GitHub issues add human data. With at least 200 such shots per RTT bucket, rewind is added if the hit rate at 100 to 200 ms RTT is 25% or more below the hit rate at under 30 ms RTT. When triggered, rewind covers direction-only manual shots only: the server keeps 200 ms of target pose history and tests the shot ray against the target's pose at the client's render tick. There is no full-world re-simulation, and track designations keep converting through the server's current track.

Every vehicle has exactly one player. When that player takes direct control of a role (the helm, a weapons group, sensors), the officer for that role switches to advisory and shows its suggested solution. Releasing control hands the role back to the officer within one graph tick. Graphs are edited only in dry dock or during a splash.

### Modes

| Mode | Players | Rules |
| --- | --- | --- |
| Campaign co-op | 2 at the first public release, 4 by 1.0 | Join between operations; reconnect within 5 minutes; time compression only while every player holds it (see Campaign) |
| Skirmish | 1 to 4 against AI | A single operation from the campaign templates, with a chosen tonnage cap and enemy size |
| Sandbox | Up to 8 | Free play with your own designs; join any time |
| PvP elimination | 1v1 and 2v2 at the first public release; 3v3 and 4v4 by 1.0 | Last team afloat; tonnage and complexity caps |
| PvP escort | 1v1 to 4v4, by 1.0 | One side runs a convoy across a 20 to 40 km map; tonnage and complexity caps |

Sandbox, skirmish and PvP have every part from first launch, with no unlock track and no campaign carry-over. Downloaded and shared blueprints can use any part, so a gated sandbox would leave them unloadable. PvP limits come from the lobby ruleset: part limits, a tonnage cap and complexity caps.

PvP rulesets:

- Standard, the default: doctrine presets, sliders and the rules table are allowed; forked officers and graph edits are banned, so every ship uses stock fire control.
- Open: forked officers and graphs are allowed.
- Both: Luau scripts never run in a PvP lobby (see Sharing and mods). The tonnage cap per hull is one of the act caps, 150, 400, 900 or 1,800 t, with 400 t the default.

Complexity points are deterministic work counts from a 10 s headless trial in the trial battery (Beaufort 4, all systems powered, firing at a drone): compound cuboids, buoyancy samples, conduit segments, compartments, graph node evaluations and Luau interrupts. Each count is multiplied by a per-unit coefficient stored in a versioned table, so that one point is about 1 single-core microsecond per 60 Hz tick on the minimum spec. The coefficients are fitted on the owner's Mac under the minimum-spec throttle profile and checked against the median of 5 runs on the hosted Windows runner. The editor gauge shows Caution above 400 points; in the campaign the gauge is information, not a cap. The PvP cap is 600 points per vessel and 3,600 points per lobby, about 1.2 ms of wall time on 3 cores, which leaves the rest of the 8 ms tick for projectiles and sensors.

- Gate (Milestone 0 spike 2, re-checked at the vertical slice): predicted points are within 20% of measured core time for the 6 starter hulls and every enemy class.
- Fallback: refit the coefficients separately per size band (10 to 30 m, 30 to 60 m, 60 to 90 m). If any hull is still more than 20% off, add a contact-pair term from the same trial. The caps stay in points.

There is no ranked play. Every host, listen or dedicated, runs the authoritative simulation and can cheat, and the project runs no servers of its own.

### Transport and hosting

An internal Transport trait has three backends. renet2 (1.0.0, October 2026) provides channels and fragmentation above all three; its single-maintainer history is the reason it sits behind the trait.

| Backend | Built | Used for |
| --- | --- | --- |
| In-memory (renet2\_netcode memory sockets) | Always | Single-player and the host's own client |
| UDP (renet2\_netcode over a native UDP socket) | Always | Every networked game before Steam: listen hosts, LAN, direct IP, dedicated servers, CI bot matches and soak tests |
| Steam Networking Sockets with Steam Datagram Relay (renet2\_steam) | Only with the `steam` cargo feature | Steam invites and lobbies, if the game goes to Steam |

A link conditioner (latency, jitter, Gilbert-Elliott burst loss, upstream limit) wraps any backend in tests.

#### Hosting and joining over UDP

The host binds UDP port 47610 by default (configurable) only when the player chooses to host, and the UI explains the operating-system firewall prompt before it appears. Joining clients connect outbound and see no prompt. Then the host maps the port, in Rust so the headless server can do the same:

1. UPnP IGD through igd-next 0.18 ([igd-next](https://crates.io/crates/igd-next)) and NAT-PMP or PCP through crab\_nat 0.8 ([crab\_nat](https://crates.io/crates/crab_nat)) start in parallel, each with a 3 s timeout. The first success wins. Godot's own UPNP class is not used, since the headless server cannot load Godot.
2. UPnP leases are 3,600 s, renewed every 1,800 s. A gateway that only accepts permanent leases gets lease 0 and an explicit delete. NAT-PMP and PCP leases are 7,200 s, renewed at half. Mappings are deleted when hosting ends; after a crash they expire with the lease.
3. If the mapped external address is private (RFC 1918) or shared CGNAT space (100.64.0.0/10), the host is behind double NAT, and the panel says outside players cannot reach it.
4. If mapping fails or double NAT is detected, the host panel shows the manual-forwarding steps from the README (UDP 47610 to this machine's LAN address) and the mesh VPN option. With Tailscale or ZeroTier running, the panel offers a join code for the VPN address. No relay server is run.

Players join with a join code or by address. The join code is Crockford base32 in groups of 4, about 34 characters for IPv4. It holds a format version, the protocol version, the address and port, a 10-byte per-session lobby secret and a 2-byte BLAKE3 checksum. Joining by address takes address:port plus the 16-character lobby key that the host panel shows, which is the same secret. The netcode private key is BLAKE3 derive\_key("Black Pennant join key v1", secret), and the joining client mints its own connect token from it. Traffic is therefore encrypted, and the secret works as the lobby password; regenerating it locks out old codes. A join code contains the host's public IP address, and the host panel says so, because the UDP path has no relay to hide it. LAN players join by the LAN address the host panel shows.

Each install has a random 64-bit client id in its user data directory, which renet2 uses as the client id and the host uses to hold a dropped player's slot for 5 minutes. Players pick a display name locally; there are no accounts. The host can kick a player and ban their client id for the session. A protocol version mismatch refuses the join with a message naming both versions. Builds from GitHub and, later, from Steam with the same protocol version can join each other over UDP.

Co-op host loss: each member also maps its own port in the background after joining. The host sends every member a succession list of members whose mapping worked, in join order, with their external endpoints and a fresh secret. When no packet arrives from the host for 10 s, the first member on the list re-hosts from the last 60 s operation checkpoint, and the others connect to it automatically. If nobody's mapping worked, each member sees a Re-host button that produces a new join code to share by hand.

Automatic mapping cannot be tested against real routers in CI. A CI integration test runs a mock IGD and a mock PCP server on loopback, and the host panel's mapping result goes into the connection log that playtest reports attach. If fewer than 70% of hosts in the first public playtest's reports map automatically, the host panel puts the VPN path first and the README moves it above manual forwarding.

#### Steam backend

Rust owns the Steam API through the steamworks crate that renet2\_steam uses, compiled only with the `steam` cargo feature, which is off by default. The Steamworks SDK is never committed; the steamworks crate's build fetches it when the feature is on. Lobbies, invites, Workshop, Cloud and auth reach the UI through the godot-rust adapter. No Godot Steam plugin is used, since two owners would pump Steam callbacks twice and the headless server cannot load a Godot plugin. The relay handles NAT traversal and never reveals player IP addresses ([Steam Datagram Relay](https://partner.steamgames.com/doc/features/multiplayer/steamdatagramrelay)). Join codes stay available in the Steam build.

#### Dedicated servers

A headless dedicated-server binary exists from the first milestone for CI and soak tests. It ships on GitHub Releases for Linux x64 and Windows x64 with every release after the first public release. It reads a RON config (port, mode, ruleset, lobby size, upload figure), maps its port the same way, and prints its join code at start. Anyone can run one. The project hosts none: pay-per-use container hosting such as Edgegap ($0.00115 per vCPU-minute, [Edgegap pricing](https://edgegap.com/pricing)) is cash spend, and Hathora's 2026 acquisition by Fireworks AI is a reminder that hosting vendors leave games. The server code stays provider-agnostic, so a container host remains possible if the owner ever approves spending.

### Overload and connection quality

The host uses one degradation ladder in every mode, the same ladder the Performance budgets section lists. Rung 1 (visual detail) runs on each client. The host's rungs are:

- Rung 2: untracked hulls go kinematic beyond 5 km, then at any range.
- Rung 3: the projectile cap drops from 2,000 to 1,500 to 1,000, and above the cap one autocannon projectile carries 2 rounds.
- Rung 4: time scale drops from 0.9x to 0.5x in steps of 0.1, with 0.5x the floor.

The host steps up one rung or sub-step when the sim tick's p95 over a 2 s window exceeds 8.0 ms, and waits at least 4 s before the next step up. It steps down when p95 stays under 6.0 ms for 10 s. Time-scale steps start only after rungs 2 and 3 are spent, and only when the tick backlog exceeds 3 ticks (50 ms behind real time). Every change shows in the HUD and the log, because silent slow-motion is the top Stormworks complaint. PvP lobbies enforce tonnage and complexity caps so the host cannot be overloaded on purpose, and every player sees the host's tick rate.

- Gate, controller logic (nightly on hosted runners, with injected synthetic tick cost): backlog never exceeds 30 ticks and returns to 3 ticks or fewer within 5 s of each time-scale step; no step down is followed by a step up within 30 s; the host returns to rung 0 within 30 s of the load dropping.
- Gate, real load (each milestone, on the owner's Mac under the minimum-spec throttle profile): the reference battle plus 50% more hulls for 20 minutes, then the extra hulls removed, with the same pass conditions.
- Fallback: if the ladder oscillates, the step-down threshold becomes 5.0 ms and the dwell becomes 30 s.

Host upstream: the host panel has an Upload speed field, 1 Mbps by default, which covers co-op, skirmish and 2v2 PvP (3 clients); a larger sandbox needs the host to enter a higher figure. The lobby's client limit is floor(0.9 x upload / 256 kbps), capped at the mode's player maximum minus the host: 1 Mbps allows 3 clients (4 players), and an 8-player sandbox (7 clients) needs about 2.0 Mbps, inside the 1.5 to 2 Mbps budget figure. In the Steam build, Steam's connection stats fill the field. If, over 10 s, median client loss exceeds 5% and median RTT has risen 50 ms or more over the lobby's first-minute baseline, the host treats its upload as saturated: every client's cap drops 25% (floor 128 kbps), the upload figure drops to the measured send rate, and new joins are refused while the HUD shows the saturation. Caps recover 10% every 30 s while loss stays under 2%. Gate (nightly, link conditioner limiting host upload to 1 Mbps with 7 bot clients): within 20 s, loss falls under 5% and no client is dropped by the rule below.

Everyone can see per-player ping, host tick rate, frames behind (snapshots the client is behind the newest one sent) and bandwidth quality, and the host drops connections that stay too poor to play, as Stormworks added in 2022:

- Default: the host drops a connection when RTT stays above 500 ms, loss stays above 20%, or the client is more than 30 snapshots (1 s) behind, continuously for 15 s. A warning shows on everyone's connection panel after 5 s. In co-op, the dropped player's officers keep running their hull, and the 5-minute rejoin applies.
- Gate (nightly bot matches over the link conditioner): a client at 300 ms, 10% loss and 50 ms jitter for 1 hour is never dropped, and a client at 600 ms or 25% loss is dropped within 20 s. In playtest connection logs, 1% or fewer of dropped clients reconnect and then play 10 minutes or more without another warning.
- Fallback: if a client is dropped wrongly in the test or in more than 1% of logged cases, the hold time becomes 30 s and the loss threshold 25%.

Every drop, warning, ladder step and port-mapping result goes to a local connection log. The diagnostics bundle that players attach to GitHub issues includes it; no telemetry service exists.

### Replays

The server records replays as its authoritative output stream: vessel poses, projectile spawn and impact events, damage events, track changes and weather changes, the same data it replicates, without any re-simulation. A replay therefore plays back identically on every platform, and a kill cam shows what happened rather than a re-run that may drift. The scripted replays behind the graphics golden captures and frame-time CI use the same format.

Format: poses at 15 Hz (every other snapshot; playback interpolates), every event, the shot log, and a replicated-state keyframe every 60 s for seeking, zstd-compressed and stamped with a replay format version. A build opens only replays of its own format version. When the version changes, CI regenerates its golden replays by running their input scenarios headless. The game keeps the 20 most recent replays plus any the player pins, in the user data directory.

- Gate (vertical slice): a 30-minute reference-battle replay is 30 MB or less on disk.
- Fallback: hulls more than 5 km from every player hull record at 10 Hz.

In PvP, the server releases replays only after the match ends, sending each client its copy on the low-priority reliable lane, and only with exterior-only blueprints for the other team, so a replay never reveals interiors that the match hid. A PvP kill cam during the match shows only what the killed player's side had detected, plus the exterior of the vessel that fired the killing shot. Input-only replays that re-simulate are used solely for same-platform CI regression.

## Technical architecture

The game is a Rust simulation that Godot draws. Every rule of the game lives in Rust crates with no Godot dependency, and Godot 4.7.2 handles rendering, UI, audio playback and input through one thin godot-rust adapter. The project runs no online services: the only backend is that Rust simulation and its headless server, which players host themselves. Crash reports go to Sentry's free Developer plan, opt-in on first launch, and Steam services sit behind the `steam` cargo feature, off by default (see Dependencies).

_Architecture diagram: in the original Claude design doc, https://claude.ai/artifact/3cQK3H8ckXzt48Dbsi8pbK_

In single-player and on a listen host, the simulation inside the client process is the authoritative server; the headless binary runs the same crates without Godot. Only the adapter crate knows Godot exists.

### Crate layout

| Crate | Contents | Depends on Godot |
| --- | --- | --- |
| sim-core | Cell grids, rigid bodies (Rapier), buoyancy, waves and the replicated weather state, projectiles, damage, flooding, splitting | No |
| sim-systems | Sensor pipeline, fire control, the graph VM and officers, Luau host, enemy AI, theatre ledger | No |
| content | Blueprint, kit and campaign formats (serde, with a RON text form), share strings, migrations, validation | No |
| net | Replication schema, Transport trait, in-memory and UDP (renet2\_netcode) backends, port mapping and join codes (see Multiplayer); the Steam backend only with the `steam` feature | No |
| hull-mesh | Cell-to-mesh conversion (greedy plus shaped cells), LODs, procedural part models (weapons, masts, details), Godot surface bytes | No |
| audio | fundsp patches for every sound effect, the alert sounds, the radio effect and the menu theme; offline rendering to WAV | No |
| server | Headless binary: dedicated server, CI soak tests, Fit Check and trial-battery worker | No |
| xtask | `cargo xtask gen`: writes the generated files (wave shader include, baked FFT detail, rendered audio) into the Godot project | No |
| gdclient | cdylib on godot 0.5.5 with api-4-7: snapshot interpolation, hull chunk mesh, volume and MultiMesh uploads, wave uniforms, input, UI bindings | Yes |

GDScript is UI glue only. Keeping almost all code engine-agnostic means cargo test runs it on the free GitHub-hosted Windows, Linux and macOS runners without a GPU, gdext's occasional breaking changes stay inside one crate, and Claude, which builds the game, works in plain Rust with cargo test as its main feedback loop.

Generated files are not committed. They live under `res://shaders/gen/`, `res://art/gen/` and `res://audio/gen/`, are listed in .gitignore, and `cargo xtask gen` rebuilds them before every editor session, test run that imports them, and export. Every generator is seeded and deterministic. CI runs it twice per platform and fails if the two outputs differ by a byte.

### Threading and the Godot boundary

The gdclient crate starts three kinds of Rust thread:

- The sim pool: exactly 3 worker threads, a rayon ThreadPool built with num\_threads(3). This is the pool the 8 ms tick budget counts (see Performance budgets).
- One meshing thread outside the sim pool, which builds hull chunk meshes (see Graphics and rendering).
- One background worker for editor gauges and splash pre-builds. Fit Check uses the background worker, plus the sim pool during briefings, when no sortie ticks.

The simulation exchanges data with the main thread through triple-buffered snapshots and an input command queue, and it never touches a Godot type off the main thread. Each frame, gdclient reads the newest snapshot, interpolates in Rust, and converts positions to f32 relative to an f64 render origin, which re-centres (snapped to 1 km) when the camera strays 4 km from it. It sets one transform per hull chunk and calls RenderingServer.multimesh\_set\_buffer once per part, projectile and VFX mesh type. Hull chunk meshes are built on the meshing thread and uploaded only when cells change (see Graphics and rendering). The player's ship and the UI use a handful of ordinary node updates.

This shape follows from gdext's threading rules. Without its experimental-threads feature, gdext touches bindings only from the main thread, and its own docs say that feature carries "a high risk of unsoundness" ([gdext March 2026 update](https://godot-rust.github.io/dev/march-2026-update/)). The experimental-threads feature is never enabled. Godot's separate render-thread model is marked experimental and not for production in 4.7, so the project keeps the default Safe model.

Two rules guard the boundary, and a CI lint enforces the second:

- No gameplay logic in GDScript or Godot nodes.
- No Godot physics for gameplay. Godot 4.6 and later default new 3D projects to Jolt; this project sets the physics engine to the dummy server, routes UI picking through Rust queries, and fails CI if a gameplay scene contains a RigidBody3D or CharacterBody3D.

### Simulation core

Physics uses rapier3d-f64 0.36.1 (the 64-bit float build; see Coordinates) with the parallel and enhanced-determinism features, with the identical feature set pinned on every build. Rapier's changelog says enhanced-determinism costs no measurable performance and stays bitwise-identical with the parallel solver for any thread-pool size ([Rapier changelog](https://github.com/dimforge/rapier/blob/master/CHANGELOG.md)). The simd8 feature is never enabled, because it does not compile together with enhanced-determinism. Each vessel's collider is a greedy-merged compound of cuboids, cached and rebuilt when cells change, so players never hand-optimise physics the way Stormworks asks them to. Rapier's automatic mass and inertia for Voxels colliders on dynamic bodies was never confirmed fixed, so the game computes mass properties itself for every collider type.

Milestone 0 spike 2 benchmarks Rapier's Voxels collider against the cuboid compounds. Voxels replaces the compounds only if both of these pass:

- It is at least 20% faster in combined collider-rebuild and contact time over a 10-minute damage soak.
- It gives identical same-platform state hashes across 10 reruns, and its contact results keep the trial battery within its tolerances.

Otherwise the cuboid compounds stay. The rule applies automatically when the spike reports.

Buoyancy, drag and projectiles are custom code on flat arrays. Vessels, crew capacity and AI live in a hand-rolled entity table, not hecs, because hecs documents its query order as arbitrary ([hecs World::query](https://docs.rs/hecs/0.11.2/hecs/struct.World.html#method.query)). The table works like this:

- Entities live in a generational slot map, EntityId { index: u32, generation: u32 }. Freed slots are reused last-in, first-out.
- Each component kind (vessel, crew capacity, AI state and so on) is one dense Vec column indexed by slot, with an occupancy bitset.
- Every system iterates in ascending slot index, and snapshots and replays serialise in that order.
- sim-core and sim-systems ban iterating std HashMap and HashSet, enforced by clippy disallowed\_types. Where a map is needed, they use BTreeMap, or IndexMap with a fixed hasher.
- A unit test runs a scripted spawn and despawn sequence and asserts the hash of the iteration order.

Hot data is structure-of-arrays: projectile arrays, and per-vessel cell grids of u16 ids in sim chunks of 16³ cells (8 m) with derived solid, watertight and air bitsets. The renderer meshes larger chunks of 32³ cells (16 m), each covering 2 × 2 × 2 sim chunks on the same grid alignment (see Graphics and rendering). Work runs in parallel per vessel with rayon and merges serially in a fixed order. Parallel float sums are banned, because rayon documents their order as unspecified.

Waves are a sum of 12 Gerstner waves (at most 16, wavelengths 8 to 400 m) seeded by the server, evaluated with libm rather than std trigonometry. The count is fixed in every sea state: sea state changes the amplitudes, wavelengths and directions drawn from the spectrum, so buoyancy cost does not depend on weather. The same parameters generate the Godot ocean shader, and a golden test compares shader heights against sim-core at sample points so ships never visibly float above or sink into the drawn water. sim-core also emits the shared shader include (res://shaders/gen/waves.gdshaderinc) and owns a replicated weather state (sea-state code, wind, cloud cover, visibility V, rain) that drives both the waves and the render presets. FFT detail is visual only: a baked normal loop with no displacement (see Graphics and rendering).

The wave count has two gates, both first run in Milestone 0 spike 2:

1. Budget: buoyancy and waves fit their 1.25 ms share of the tick in the reference battle at Beaufort 7. This is an absolute gate, re-measured on the owner's Mac at each milestone.
2. Accuracy: in a headless test that CI re-runs on every pull request, the RMS heave and pitch of the 6 starter hulls with 12 waves are within 10% of a 32-wave reference spectrum at Beaufort 4 and 7.

The fallback applies automatically:

- If the budget fails, use 8 waves.
- If accuracy fails and the budget has room, use 16 waves.
- If both fail, the budget wins with 8 waves.

Any reduced set keeps the components with the most spectral energy. The ocean shader always uses the same set as sim-core, and the golden height test stays.

### Coordinates

The theatre is an abstract graph with f64 positions. Each sortie spawns one physics world at most 100 km across, centred on the engagement. The campaign runs one operation at a time, so only one physics world ever exists. The simulation stores positions in f64 and runs rapier3d-f64 for two reasons:

- f32 resolves only about 4 mm at 50 km from the origin, coarser than the 2 mm the network quantizes to.
- One origin cannot sit near every ship in a spread-out battle.

With 40 to 80 compound bodies the f64 cost is expected to be small. Spike 2 measures it against an otherwise identical f32 build, in the reference battle and the co-op reference battle. The project keeps f64 if either condition holds in both scenes:

- the f64 overhead in p99 tick time is 15% or less on both hosted runners;
- the f64 build meets the 8 ms p99 tick budget under the min-spec throttle profile.

Otherwise it adopts the fallback automatically. The fallback gives each cluster of nearby vessels its own f32 Rapier world with its own origin, since ships kilometres apart never touch:

- Clusters form by single linkage at 5 km. Two clusters merge when any of their vessels come within 5 km of each other, and a cluster splits only when the gap reaches 6 km.
- Each world's origin re-centres, snapped to 1 km, when any member strays more than 4 km from it.
- Projectiles, buoyancy and cell grids stay in f64 in either case.

The client renders with a floating origin on stock single-precision Godot. An f64 render origin re-centres, snapped to 1 km, when the camera is more than 4 km from it, and ocean wave phases are recomputed in f64 relative to it each frame. Godot's double-precision build was rejected because gdext would then need custom editor and export-template builds for both platforms on every Godot release ([large world coordinates](https://docs.godotengine.org/en/stable/tutorials/physics/large_world_coordinates.html)).

### Determinism

The simulation aims for same-platform reproducibility. These make it hold:

- a fixed tick;
- enhanced-determinism Rapier and libm;
- the counter-based RNG;
- canonical body insertion order and ascending-slot iteration;
- per-tick budgets counted in work units (cells visited, interrupts) rather than elapsed time.

That buys regression tests, replays and desync detection. Nothing in the netcode depends on bit-identical results across Apple Silicon and x86.

A cross-platform state-hash comparison runs nightly between the hosted macOS arm64 and Windows x86\_64 runners, hashing each subsystem on every tick over the regression scene set. It starts non-blocking:

- Before promotion, a divergence makes the nightly job open an issue automatically, naming the first diverging tick and subsystem. Merges are not blocked.
- The comparison becomes a pull-request merge gate after 30 consecutive nightly runs with zero divergence. The count restarts on any upgrade of Rapier, libm, Rust, mlua or a compiler flag.
- After promotion, a divergence blocks the PR.
- If a promoted gate shows more than 2 divergences in 30 days that are not traced to project code, it is demoted to nightly automatically and the 30-run count restarts.

### Measurement for the gates in this section

Free GitHub-hosted runners stand in for owned hardware. For public repositories these are windows-latest (x64, 4 vCPU, 16 GB) and macos-latest (arm64, 3 M1 vCPU, 7 GB) ([GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). Shared virtual machines are too noisy for absolute timings but fine for comparisons, so the gates split this way:

- Relative gates run as same-job A/B on both hosted runners, with the sim pool fixed at 3 threads. The two builds or configurations run interleaved, 5 runs each, and the gate compares medians. A relative gate passes only if it passes on both runners. This covers the f64 against f32 overhead, Voxels against compounds, the 12-wave against 32-wave accuracy test, and every hash comparison.
- Absolute gates run on the owner's Apple Silicon Pro/Max Mac under the min-spec throttle profile, which approximates the base M1 and runs a 3-thread sim pool (see Platform support). This covers the 8 ms p99 tick and the 1.25 ms buoyancy and waves share, with p99 taken over a 10-minute run with per-subsystem tracing spans.
- The Windows minimum spec is checked through community reports from the in-game benchmark scene, which runs the reference battle and reports the same p99 figures. A report that misses a budget feeds the degradation rules in Performance budgets; it does not reopen these defaults.

### Dependencies

Pin exact versions in the workspace Cargo.toml and commit Cargo.lock:

| Dependency | Pin | Notes |
| --- | --- | --- |
| godot | `{ version = "=0.5.5", features = ["api-4-7"] }` | Default features stay on: they generate every engine class, including RenderingServer |
| rapier3d-f64 | `{ version = "=0.36.1", features = ["parallel", "enhanced-determinism"] }` | Never simd8 |
| renet2, renet2\_netcode | `=1.0.0` each | renet2\_netcode is the plain-UDP backend and pulls in renetcode2 1.0.0 |
| mlua | `{ version = "=0.12.2", features = ["luau"] }` | No JIT; see the scripting rules in Automatic systems and control layers |
| binary-greedy-meshing | `=0.6.2` | MIT; used by hull-mesh |
| fundsp | `=0.23.0` | MIT OR Apache-2.0; used by the audio crate |
| renet2\_steam, steamworks | `=1.0.0`, `=0.13.1` | Optional, only with the `steam` feature; steamworks 0.13.1 is the version renet2\_steam 1.0.0 requires |
| Sentry Godot SDK | 2.5.1, vendored into addons/ at a fixed tag | Requires Godot 4.5 at minimum |
| Godot editor and export templates | 4.7.2-stable | Custom export templates with separate debug symbols, built by a hosted-runner CI job at each Godot upgrade |

An api-4-7 extension loads in Godot 4.7.2, because gdext extensions load in any Godot runtime at or above their API level ([godot-rust compatibility](https://godot-rust.github.io/book/toolchain/compatibility.html)). A scratch cdylib with the godot, rapier3d-f64, renet2, renet2\_netcode, renet2\_steam and steamworks pins resolved to 133 packages and passed cargo check on rustc 1.98.0, aarch64-apple-darwin, calling RenderingServer::multimesh\_set\_buffer. Milestone 0 adds four proofs, each a CI job on the hosted runners unless stated, with its fallback:

| Proof | Pass | Fallback, applied automatically |
| --- | --- | --- |
| The full pin set builds | Every pin above, `steam` feature off and on, builds and passes cargo test on Rust 1.99.1 on all three runners | Hold the failing dependency at the newest patch release that builds, behind the full CI matrix |
| The linked cdylib loads | Godot 4.7.2 loads it and the headless smoke test passes on Windows and macOS | Move to the newest gdext 0.5.x patch that loads, behind the full CI matrix |
| Static C runtime | x86\_64-pc-windows-msvc with +crt-static links and the exported game starts | Link the C runtime dynamically and ship the app-local Visual C++ runtime DLLs beside the executable |
| Ad-hoc signed macOS bundle | In CI, codesign -s - signs the app and its Rust library, codesign --verify --deep --strict passes, and the bundle launches headless. On the owner's Mac, for every release, a quarantined download of the release archive launches after Open Anyway | Sign the Rust library separately before signing the bundle, then re-run both checks |

The `steam` feature exists on net, gdclient and server and is off by default. Phase 1 release builds never enable it. The Steamworks SDK is never committed: the steamworks crate's build fetches it when the feature is on. One weekly CI job on the Linux runner builds with `--features steam` (build only, since tests would need a running Steam client), so the phase 2 path does not rot.

Rust is pinned to 1.99.1 in rust-toolchain.toml, with the workspace's rust-version set to match so Cargo's MSRV-aware resolver agrees:

```toml
[toolchain]
channel = "1.99.1"
components = ["rustfmt", "clippy"]
targets = ["aarch64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
```

Version floors:

- The highest floor any dependency sets is 1.94, from godot 0.5.x.
- 1.99.0 is never used, because it miscompiles extern "C" functions that return bool on x86\_64 ([rust-lang/rust#163911](https://github.com/rust-lang/rust/issues/163911)), which is exactly gdext's FFI on Windows.

Toolchain bumps follow two rules:

- A bump moves to a newer stable after its first point release, or two weeks after its release, whichever comes first. The next candidate is 1.100.0.
- A bump lands behind the full CI matrix.

Hot reload (reloadable = true) is for development only, and code that hot-reloads avoids typed signals, whose callables become undefined behaviour after a reload. Upgrades to gdext, Godot, Rapier, mlua, renet2, fundsp and binary-greedy-meshing land only behind the full CI matrix on the hosted Windows, Linux and macOS arm64 runners.

### Art and audio

The look is geometric and readable from the plot table to the bridge camera. Hulls are built from 0.5 m cells with wedges and corners, in flat-shaded steel and tonal naval greys. What makes them beautiful is a low sun, haze, cell-derived AO and edge shading, a sim-matched sea and a restrained post stack, not smoother geometry or texture work. Hulls carry no team colour, since side shows only through HUD stance symbols.

Every asset is made in code:

- Hull and part meshes are generated from blueprint data.
- The few bespoke models (weapons, masts and a small set of details) are built procedurally in the hull-mesh crate from parameters and are never modelled by hand.
- Icons are hand-written SVG.
- Any still image, including a future store capsule, is an in-engine render.

No generative image, audio, voice or music model is used, and no third-party asset library, CC0 included. For Claude working in code, procedural meshes, a good ocean shader and strong VFX for splashes, fire and flooding buy more than hand-modelled ships, and every asset can be changed and regenerated from the repository. Graphics and rendering specifies the renderer, post stack, quality tiers, budgets and how each is verified.

Audio is code too. The audio crate synthesizes these sounds with fundsp:

- every sound effect;
- the alert sounds, with one distinct pattern per sounded alert priority (Interface and visual design sets the priorities and their patterns);
- the radio effect (squelch, static and band-pass) that accompanies officer and ace messages, which are text and never voiced;
- one short menu theme.

`cargo xtask gen` renders them offline to 48 kHz, 16-bit WAV files under res://audio/gen/. Nothing is synthesized live. Godot plays the files, and Rust sets pitch and volume from sim state for engines, sea and weather. There is no music in sorties.

## Platform support

Windows is the platform machines test on every change, because Claude develops on the owner's Mac and nobody on the project owns a Windows PC. Windows is 93.70% of Steam users and macOS 2.35% ([Steam Hardware Survey, October 2026](https://store.steampowered.com/hwsurvey/)), so the platform the project sees least is the one nearly every player uses.

### Targets

| Platform | Status | Build |
| --- | --- | --- |
| Windows 10 and 11, x64 | Supported; released on GitHub Releases | MSVC target with a static C runtime (+crt-static), so no runtime redistributable to ship |
| macOS on Apple Silicon | Supported, macOS 14 or later; released on GitHub Releases | arm64-only app; the Rust library is built for aarch64-apple-darwin with MACOSX_DEPLOYMENT_TARGET=14.0 and packaged as a .framework |
| macOS on Intel | Not built | An Intel slice would run Godot on Vulkan through MoltenVK instead of Metal, untested here |
| Linux x64 | Dedicated server supported, glibc 2.31 or later; client built in CI but not published | Native build inside the Steam Runtime 3 sniper SDK container |
| Steam Deck, Windows on ARM | Not at the first public release | Revisit after it |

Intel Macs are under 1% of Steam Macs, and M1 alone is 14.22% ([Mac survey](https://store.steampowered.com/hwsurvey/?platform=mac)). Native arm64 is required anyway: from macOS 28, Apple keeps Rosetta only for older, unmaintained games ([Apple support](https://support.apple.com/en-us/102527)). Releases are arm64-only, so there is no Universal 2 build or lipo step.

The macOS floor is macOS 14 Sonoma. The export preset sets application/min_macos_version_arm64 = "14.00", up from the Godot 4.7.2 default of 13, because 4.7.x exports with baked shaders abort at start when the minimum is below 14 ([#123266](https://github.com/godotengine/godot/issues/123266)). Every Apple Silicon Mac can run macOS 14. The README lists "macOS 14 Sonoma or later, Apple M1 or later". At each Godot minor upgrade the floor drops to 13 only if #123266 is fixed and the clean-cache launch test passes at 13.00.

The Linux server is x86_64 and needs glibc 2.31 or later. It is built inside Valve's Steam Runtime 3 sniper SDK container (registry.gitlab.steamos.cloud/steamrt/sniper/sdk, Debian 11, glibc 2.31) with the pinned Rust toolchain. Not ubuntu-latest, and not steamrt4, whose glibc 2.41 fails on Ubuntu 24.04 (2.39) and Debian 12 (2.36); not static musl, because libsteam\_api.so (steam feature) is a glibc library needing only GLIBC\_2.14. CI fails if the server binary's highest GLIBC\_ symbol version is above 2.31 (objdump -T | grep -o 'GLIBC\_[0-9.]\*' | sort -V | tail -1), and smoke-runs the binary in debian:11 and ubuntu:24.04 containers. Server docs list Ubuntu 22.04 and 24.04 LTS and Debian 12 and 13, and "any x86\_64 Linux with glibc 2.31 or later". An OCI image of the same binary on debian:12-slim serves soak tests only.

The game uses the Forward+ renderer on every platform. project.godot pins every renderer key, and a CI lint fails if any is missing or changed: rendering/renderer/rendering\_method = "forward\_plus", rendering/rendering\_device/driver.windows = "d3d12", driver.macos = "metal", driver.linuxbsd = "vulkan", and fallback\_to\_vulkan, fallback\_to\_d3d12 and fallback\_to\_opengl3 all true. driver.windows is set explicitly because the engine's class default is still Vulkan and only projects created in 4.6 or later get D3D12 written to project.godot. It falls back to Vulkan, then OpenGL 3 ([Godot renderers](https://docs.godotengine.org/en/4.7/tutorials/rendering/renderers.html)). Graphics spike S2 makes Vulkan the default if D3D12 is more than 10% slower or hitches; with no owned Windows GPU, S2 is answered by community benchmark reports (see Rendering coverage).

OpenGL 3 is a safe mode, not a supported quality tier. When no D3D12, Metal or Vulkan device works, Godot switches the whole rendering method to Compatibility, which has no compute shaders, TAA, FSR2, SSR, volumetric fog or decals and lights in nonlinear sRGB. Machines that reach it are below the minimum spec, usually from an outdated driver. Safe mode boots, renders the ocean with Gerstner displacement only (no compute FFT detail), keeps every gameplay cue, skips art review, and shows a persistent notice: the GPU driver lacks Direct3D 12 and Vulkan support, update the driver, this mode is unsupported. At startup the game logs RenderingServer.get\_current\_rendering\_method(), get\_current\_rendering\_driver\_name(), get\_video\_adapter\_name() and get\_video\_adapter\_api\_version(), plus every fallback, since a silent fallback can leave a build that runs but looks wrong; the values go into crash and benchmark reports.

### Distribution and signing

Phase 1 ships free on GitHub Releases: a Windows x86\_64 zip, a macOS arm64 zip of the .app, Linux and Windows server archives after the first public release, SHA256SUMS.txt, and a build-provenance attestation per asset (actions/attest-build-provenance), free for public repositories and checkable with gh attestation verify ([artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)). Versions stay 0.x until 1.0. A tag vX.Y.Z-beta.N publishes as a GitHub pre-release, the beta channel; public pre-releases open at the vertical slice. Only CI builds release assets, from tagged commits, except under the macOS fallback in Rendering coverage.

No signing is bought: Microsoft Artifact Signing ($9.99 a month), OV or EV certificates and the Apple Developer Program would break the zero-spend rule.

macOS builds are ad-hoc signed (codesign -s -), with no Developer ID, no hardened runtime and no notarization. Arm64 code must be signed to run; ad-hoc suffices. Godot's export signing is off; CI signs each nested Mach-O (the gdclient framework, any dylib) inside out, never with --deep, then the bundle, and fails unless codesign --verify --strict passes on all of them. Without the hardened runtime, library validation does not apply, so the GDExtension needs no Disable Library Validation entitlement. Gatekeeper refuses a quarantined download's first launch; the README says to open the app once, then use System Settings > Privacy & Security > Open Anyway. macOS 15 and later removed the Control-click bypass; on macOS 14, Control-click > Open also works. Each new version asks again once.

Windows builds are unsigned. A downloaded zip carries the Mark of the Web, so SmartScreen shows "Windows protected your PC"; the README documents More info > Run anyway. Windows 11 Smart App Control blocks unsigned executables without reputation whatever the download source, Steam included ([SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)). It is on by default only on clean installs and has no per-app exception, so the README says such machines cannot run the game until the player turns it off.

Phase 2, only if the owner judges the game really good, puts it on Steam for free. Steam builds turn on the steam cargo feature, which links libsteam\_api; the Steamworks SDK is never committed. libsteam\_api.dylib keeps Valve's signature and file name; the rest is ad-hoc signed as above. The Steam client writes game files itself, so they carry no quarantine attribute or Mark of the Web and skip the Gatekeeper and SmartScreen prompts; Smart App Control still blocks the unsigned exe. Phase-2 gate: on a Steam-installed build, xattr -l shows no com.apple.quarantine on the app, and on a clean macOS user account it launches, calls SteamAPI\_Init and reaches the main menu. Fallback: if the attribute is present, the store page and first-launch notes explain the one-time Open Anyway step. Signing stays a known limitation on Steam unless the owner approves spending. Servers then also ship through SteamCMD as a Tool app open to anonymous logins ("Include this tool in Dedicated Server package"). Weekly CI compiles the steam feature and publishes nothing from it.

### Automated coverage

CI runs on GitHub Actions standard hosted runners only, free and unlimited on public repositories ([runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing), [hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). No larger runners, cloud GPU instances, bought hardware or self-hosted runners, which GitHub says should almost never serve public repositories because any pull request can compromise them ([secure use](https://docs.github.com/en/actions/reference/security/secure-use)). Images are pinned by label: windows-2025 (4 CPUs, 16 GB), ubuntu-24.04 (4 CPUs, 16 GB) and macos-15 (3 M1 cores, 7 GB), plus macos-26 weekly. Image upgrades land as pull requests, as goldens are keyed on the image. The free plan allows 20 concurrent jobs, 5 of them macOS, and 6 hours per job ([limits](https://docs.github.com/en/actions/reference/limits)).

| When | Windows | Linux | macOS |
| --- | --- | --- | --- |
| Every pull request (gates merge) | Build, cargo test, sim regression against per-platform golden files, trial battery, headless smoke test, GdUnit4 unit suite, renderer-key lint | Build, cargo test, dedicated-server soak (2 bots, 5 minutes), GdUnit4 UI suite under a virtual display | Build, cargo test, trial battery matching Windows within tolerances, headless smoke test |
| Merge to main | All of the above, plus the release export with D3D12 shader baking, the manifest check and the long-path save test | All of the above, plus the sniper-container server build, glibc check and container smoke runs | All of the above, plus the release export with Metal baking, ad-hoc signing and the clean-cache launch test |
| Nightly | Cross-platform state-hash comparison with macOS; mixed Mac and Windows bot matches over the UDP transport under simulated loss, jitter and latency; occluded-window timer test; pipeline-compilation check on WARP | Server soak, 8 bots, 2 hours | State hashes and the macOS side of the mixed matches |
| Weekly and on release tags | Golden captures under d3d12 (WARP), vulkan (lavapipe) and opengl3 (llvmpipe); window-mode and controller-input scripts; steam-feature build | Golden captures under vulkan (lavapipe) | Golden captures on Metal; macos-26 run; steam-feature build; manual play on Apple Silicon |
| Release tags | Publish assets, checksums and attestations; needs a passing owner's-Mac bench result | Publish the server tarball | Publish the app zip |

Each pull-request job has a 30-minute median budget over 7 days, checked weekly; over it, its slowest step moves to merge-to-main until the median recovers.

The headless smoke test runs the export with --headless: 3,600 ticks (60 s) of the splash basin at Beaufort 7, the 22 m fast attack craft against the patrol boat and drones, then 1,800 ticks of a seeded Act 1 convoy raid loaded through the campaign path. It fails on any error-level log line, Rust panic, Godot script error or non-zero exit. Gate: under 5 minutes of wall time on the Windows runner. Fallback: if over, the operation segment drops to 900 ticks, then the splash to 1,800. The editor's trial battery and the blueprint migration corpus run in the same jobs, so a ship that floats on a Mac and sinks on Windows fails a pull request.

Trial-battery tolerances for macOS against Windows: turning-circle diameter and time to 90% of top speed within 1%, heel recovery time and peak heel within 2%, top speed within 0.5%. The chaotic one-against-three fight is compared statistically over the same 10 seeds: at least 9 of 10 outcomes match and mean remaining hull fraction is within 5 percentage points. Fallback: a tolerance that fails more than once in 50 pull-request runs with no code cause doubles once (scalars to 2% and 4%, outcomes to 8 of 10); if it still fails without a cause, that metric moves to the nightly job.

Server soaks pass with zero error-level log lines, zero desyncs and, nightly, resident memory growth under 5% from minute 10 to the end. The nightly mixed matches join a Windows host job and a macOS client job through ephemeral Tailscale nodes (free Personal plan, auth key in a CI secret), a route the README offers players, and pass with zero desyncs and disconnects. Fallback: after 3 consecutive nights without a connection, they switch to a recorded trace: the Windows job hosts a loopback client and records every packet, and a macOS client replaying the host stream must decode every packet and match the Windows client's per-tick state hashes.

The GDScript suite uses GdUnit4 (MIT), vendored in addons/gdUnit4 at a pinned 6.2.x release (6.2.3 on 12 November 2026; its compatibility table lists Godot 4.7 and 4.7.1). It covers UI glue only; tests load the real gdclient library and feed it scripted snapshots through a test-only Rust entry point instead of mocking Rust classes. The Windows job runs `godot --headless --path . -s res://addons/gdUnit4/bin/GdUnitCmdTool.gd --ignoreHeadlessMode -a res://test/unit`, writes JUnit XML and gates on exit code 0. GdUnit4 6.x delivers no InputEvents headless, so scene-runner tests that simulate input live in res://test/ui and run in the Linux job through godot-gdunit-labs/gdUnit4-action@v1 with a virtual display. When GdUnit4 7.x is stable (it supports scene-runner input headless), the suite upgrades behind the full matrix and res://test/ui also runs on Windows.

### Rendering coverage

Hosted runners have no GPU and run Windows Server 2025, not consumer Windows 11; --headless skips the renderer. Three layers cover rendering: software rasterizers check correctness, the owner's Mac checks GPU cost under a minimum-spec profile, and community benchmark reports verify the minimum spec on real hardware.

Software rasterizers. On Windows, DXGI lists only the Microsoft Basic Render Driver (WARP), a software adapter that Godot 4.7 accepts as a CPU device and picks only when no hardware GPU exists ([D3D12 context driver](https://github.com/godotengine/godot/blob/4.7/drivers/d3d12/rendering_context_driver_d3d12.cpp)). Vulkan runs on Mesa's lavapipe and OpenGL 3 on llvmpipe, from Mesa's prebuilt Windows binaries pinned by SHA-256. Linux runs distro lavapipe under a virtual display; macOS runs Metal on the VM's paravirtual GPU. Runs use a window, --rendering-driver, --write-movie and --fixed-fps 60, so slow rasterizers produce the same frames as fast ones. They check that each driver boots, that a forced failure walks and logs the fallback chain, that safe mode renders, and that RENDERING\_INFO\_PIPELINE\_COMPILATIONS\_DRAW (a count, not a time) does not rise after load. They never gate frame time.

Golden captures, at fixed frames of scripted replays: (1) the bridge view, calm, Beaufort 4, dusk; (2) the plot table with 40 tracks and uncertainty shapes; (3) the editor X-ray with conduit and arc overlays; (4) a battle frame with fire, flooding, smoke and splashes; (5) the splash basin at Beaufort 7. The Graphics and rendering capture matrix runs in the same jobs. Goldens are keyed by driver, rasterizer and runner image; safe mode keeps its own. A capture fails above a mean NVIDIA FLIP of 0.05 over the full frame, or if under 99.9% of masked UI-region pixels are within 8 of 255 per channel. A new golden needs the owner's approval. Fallback: a scene that fails in 2 of the last 8 weeks with no rendering bug found gets its FLIP limit raised to 0.08 once; if it still flakes, that capture moves to a static-camera frame of the same scene.

Milestone 0 spike 4 gates the software layer. Gate: each software driver renders all five scenes, two runs of one commit differ by a mean FLIP of 0.01 or less, the non-headless editor on the Windows runner completes a D3D12 export with baked shaders, and each driver's rendered suite finishes in under 60 minutes. Automatic fallbacks, recorded in ci/drivers.toml:
- A driver that fails to start or repeat itself leaves that platform's matrix. If neither d3d12 on WARP nor Vulkan on lavapipe renders Forward+ on Windows, Windows keeps only the fallback-chain and safe-mode checks, and the Forward+ goldens come from Linux and macOS.
- A suite over 60 minutes drops to scenes 1, 2 and 4 on that driver.
- If the Windows runner cannot bake D3D12 shaders, Windows releases ship unbaked behind the precompile screen.
- If Metal does not start in the hosted macOS VM, the Metal goldens, the baked export, signing and the clean-cache launch test run on the owner's Mac through the repo's local release script, which uploads the macOS asset with gh release upload.

The owner's Mac. The owner's Apple Silicon Pro or Max Mac runs GPU performance gates from a local script, not as a registered runner, under a bench profile that approximates the base M1 minimum spec: Low tier settings, a 3D render scale reduced to match a base M1's GPU load, and the worker pool capped at the tick budget's 3 workers. Its scale and CPU factors start from published chip specifications and are refit from community base-M1 reports. Timing uses Instruments' Metal System Trace or MTL\_HUD\_ENABLED=1, never Godot's GPU timer, unusable on Metal in 4.7. Gate: a release tag needs a bench result for its commit under bench/results/ in which the reference-battle replay holds p95 frame time at 24.0 ms or less (the Low tier's cold budget, leaving room for the 0.72 thermal derate), at most 1% missed vblanks at 30 fps, and the 2 ms main-thread adapter budget. It also runs weekly while rendering code changes.

Community benchmark reports. A main-menu Benchmark entry (or --benchmark) replays the reference battle on a fixed camera path for 60 s, with an optional 20-minute soak. Its plain-text report holds build id, OS, CPU, RAM, GPU adapter and driver version, rendering method, driver and fallbacks, tier, window size and 3D scale, p50, p95 and p99 frame time, GPU time on D3D12 and Vulkan, simulation tick p95, pipeline compilations after load and, on Macs, thermal-state history. It holds no user names or paths and is never sent automatically. Players paste it into a GitHub issue form; a workflow files it under bench/community/, which generates the README hardware table.
- The minimum spec counts as confirmed for a class (RTX 2060 or RTX 3050 6 GB with a 6-core CPU and 16 GB; base M1 with 8 GB) once at least 3 reports from distinct machines in that class meet its target at p95: 16.7 ms frame time and 14.5 ms GPU time at 1080p on Windows, 33.3 ms frame time on the M1. Until then the README shows it as the design target with the count of confirming reports. If reports show a class missing its target, auto-detect maps that class one tier lower and the minimum spec does not rise.
- The 20-minute soak replaces an owned MacBook Air: with 3 or more Air reports, sustained p95 must stay within the Low target; with fewer, the Low budget's 0.72 thermal derate and the runtime thermal-state response stand in.
- If 3 or more base-M1 reports of one build differ from the bench profile's p95 by more than 15%, the profile is refit to their median.
- Spike S2 switches the Windows default to Vulkan when at least 5 reports from distinct NVIDIA or AMD machines at or above the minimum spec, run in both drivers on one build, show D3D12's median p95 more than 10% slower than Vulkan's, or show hitches (frames over 50 ms after load) under D3D12 but not Vulkan on 2 or more machines. With fewer than 5 such reports, D3D12 stays the default.

Driver cohort. Each pre-release wave asks, in a pinned GitHub Discussion, for 25 Windows machines on Intel's graphics driver (UHD, Iris Xe, Arc) and 25 on AMD integrated graphics (Vega, RDNA 2 and 3 APUs) or older AMD cards (GCN, Polaris). Testers check launch, renderer selection and fallbacks, the five golden scenes, display scaling at 125% and 150%, and a mixed-scale second monitor. The bug-report dialog shows adapter and driver version for pasting. These machines are below the minimum spec (6 GB VRAM), so cohort results never gate a performance budget. Proton and Steam Deck reports do not count: Proton translates D3D12 and skips Windows drivers. A boot failure reported by 2 or more testers, or seen in 3 or more crash events, on an Intel or AMD driver released in the last 12 months blocks promotion of that pre-release.

### Crashes and privacy

The Sentry Godot SDK (2.5.1, Godot 4.5 or later) ships from the first public pre-release, on Sentry's free Developer plan: $0, one user, 5,000 errors a month, 30-day lookback ([Sentry pricing](https://sentry.io/pricing/)). The organisation lives in Sentry's EU region (Frankfurt), fixed at creation ([data storage location](https://docs.sentry.io/organization/data-storage-location/)). No separate telemetry service exists; hardware context rides in Sentry events.

Consent is opt-in. The first-run screen asks one question, whether to send crash reports, with equal-weight Send and Don't send buttons and nothing preselected, and lists a report's contents: stack trace, minidump, OS, GPU model and vendor, driver version, rendering method and driver, fallback history, quality tier, build id, host or client role, pipeline compilations after load and a random installation id. Settings > Privacy has the same toggle, effective at once. One flow serves all regions. Nothing is written or sent before the choice: sentry/options/auto\_init is false, so the SDK, its installation\_id file and crash database start only after Send. attach\_log is false, because the Godot log contains the user name in paths, and send\_default\_pii stays false. Withdrawing consent closes both SDKs and deletes the Sentry database directory and the installation\_id file.

Quota: before\_send drops repeats of a fingerprint on one install within 24 hours and adds the suppressed count to the next event sent. Gate: accepted errors stay under 4,000 in a calendar month. Fallback: if a month goes over, the next release sets sample\_rate to 0.5 for non-fatal errors, keeping fatal crashes at 1.0, and halves it again each month the gate still fails.

Rust panics reach Sentry through the sentry crate (0.49.5) running beside the Godot SDK, whose script API cannot attach a Rust stack.
- The Godot SDK owns native crashes, Rust segfaults and aborts included (sentry-native with crashpad\_handler.exe on Windows, the Cocoa SDK on macOS). The Rust crate owns panics only: `sentry = { version = "0.49.5", default-features = false, features = ["panic", "backtrace", "contexts", "debug-images", "ureq", "rustls"] }`. Never minidump, which would re-execute the Godot binary as a crash reporter beside Crashpad; release-health stays off, so only the Godot SDK tracks sessions.
- Init: after consent, GDScript calls SentrySDK.init(), then a gdclient #[func] calls sentry::init with the same DSN, release, dist and environment. The panic integration chains to gdext's hook, so panics still print to the Godot log.
- Deduplication: the Godot SDK's before\_send drops events whose first exception value starts with gdext's "[panic " prefix; the Rust event counts. One gdclient function sets each tag in both SDKs.
- panic = "unwind" stays, because gdext catches panics at #[func] boundaries. A worker panic is reported and handled as a host loss; a panic inside the hook aborts and the native SDK records it. The ClientInitGuard drops in on\_level\_deinit, flushing for up to 2 s.
- Milestone-1 test on Windows and macOS: a debug command panics once on the main thread and once on a worker; Sentry must show one issue per panic, with symbolicated Rust frames and the renderer tag.

Symbols: every tagged build strips debug info from the Rust library and uploads PDB and dSYM with sentry-cli debug-files upload. Symbolicated engine frames need custom Godot export templates built with separate debug symbols ([Sentry for Godot](https://docs.sentry.io/platforms/godot/configuration/stack-traces)). A CI job on the free hosted runners builds Windows x86\_64 and macOS arm64 templates with separate symbols at each Godot upgrade, stores them as assets of a templates release in the repo, and uploads their symbols to Sentry. Gate: each platform's build finishes inside the 6-hour job limit. Fallback: a build that runs over splits into one job per template target and restores its SCons cache; if Sentry refuses the engine symbols on the free plan, they stay as release assets for local symbolication and only Rust symbols upload.

A short privacy policy, in the repo and on the GitHub Pages site, ships before the first build that can send a report, since GDPR applies to EU players. It names Sentry as processor, lists the report contents above, states the 30-day retention and explains how to withdraw.

### Release promotion

Every release spends at least 7 days as a GitHub pre-release (in phase 2, a Steam beta branch follows the same rule). Metric: Windows crashes per 1,000 sessions in Sentry, a session being one launch by an opted-in player, over the first 7 days. With at least 500 Windows sessions, it promotes when its rate is not significantly higher than the previous release's first-7-day rate (one-sided Poisson rate test, p < 0.05) and no new crash group has 3 or more events. It is evaluated automatically after 7 days, then daily; a failed test blocks promotion and the next build restarts the window. Fallback, the usual path while few opt in: without 500 sessions in 14 days, it promotes when no new crash group has 3 or more events, the full CI matrix and the owner's-Mac bench pass on its commit, and no driver-cohort boot failure blocks it. Security hotfixes due within the 72-hour Luau window skip the 7 days and promote once the full CI matrix passes.

### Windows failure classes

| Failure class | Mitigation |
| --- | --- |
| GPU driver variety (Intel integrated, older AMD) | Hardware context in crash reports, the driver cohort, community benchmark reports |
| Shader compilation stutter | Godot ubershaders; the shader baker for D3D12 and Metal exports (Metal with a minimum of macOS 14 and a clean-cache launch test in CI, shipping unbaked if issue [#124539](https://github.com/godotengine/godot/issues/124539) reproduces); a warm-up scene that draws every material and VFX variant at load; a CI gate on RENDERING\_INFO\_PIPELINE\_COMPILATIONS\_DRAW; pipeline-compilation monitors in the performance HUD |
| Non-ASCII user names, OneDrive-redirected Documents | Save under the user data directory, never lossy UTF-8 path conversion, CI tests with a non-ASCII profile path |
| Paths over 260 characters | Godot 4.7.2's stock manifest, and the one the export writes with application/modify\_resources on, declare longPathAware, so no custom template, rcedit or mt.exe edit is needed, and nothing edits the exe after export. The entry needs LongPathsEnabled = 1, off by default, so the real mitigation is prefixing: Godot's FileAccessWindows and DirAccessWindows prefix absolute paths with \\\\?\\, and Rust std converts paths over 248 UTF-16 units; files open only through std::fs or Godot FileAccess and DirAccess. DLL loading and Crashpad's database are not prefixed, so those paths stay short (game directory, user data directory). CI extracts the manifest (mt.exe -inputresource:BlackPennant.exe;#1) and fails if longPathAware is missing; the non-ASCII-profile test also saves and loads at a path over 260 characters with LongPathsEnabled = 0. |
| Antivirus and SmartScreen warnings on new unsigned Rust binaries | No packers, stable binary names, checksums and attestations on every release, README instructions, and false positives submitted to Microsoft within a day of a report |
| Firewall prompts for listen servers | A direct-IP host binds only when the player hosts, maps its port with UPnP IGD or NAT-PMP/PCP, and the UI explains the firewall prompt before it appears; joining is outbound and needs no prompt. Phase-2 Steam relay is outbound too. |
| Timer resolution when the window is hidden | Windows 11 drops timer guarantees for occluded windows, so a Rust clock thread in gdclient, not Godot's main loop, drives the 60 Hz tick from absolute Instant deadlines (start + n × 16,666,667 ns), runs up to 4 owed ticks per wake before dropping the backlog (counted in the host tick-rate HUD), and sleeps to 1.5 ms before each deadline, then spins. While hosting, SetProcessInformation with ProcessPowerThrottling opts out of execution-speed and timer-resolution throttling (ControlMask EXECUTION\_SPEED \| IGNORE\_TIMER\_RESOLUTION, StateMask 0), reset when hosting ends; macOS holds an NSProcessInfo activity (UserInitiated \| LatencyCritical) against App Nap. Nightly on the Windows runner: 10 minutes minimized, then 10 covered; pass if at most 0.1% of ticks start over 2 ms late and the tick count matches wall time within 1 tick. Fallback: failing on more than 1 of 10 nights with no code cause relaxes the late-tick limit to 1%; the tick-count check stays. |
| High DPI, multiple monitors, exclusive fullscreen, controllers | Window-mode scripts (windowed, borderless, exclusive fullscreen, resolution changes) with screenshot comparison on the Windows runner's display each week; DPI scaling and mixed-scale second monitors are on the driver-cohort checklist. Gamepads play sorties, menus and the chart table fully; the editor stays keyboard and mouse. The smoke test injects joypad events through Input.parse\_input\_event, and the owner checks a physical controller on the Mac before each tag. |
| DLL loading | The GDExtension and Steam libraries load from the game directory only |

The residual risk is real. CI covers software rasterizers only and the project owns no Windows GPU, so NVIDIA, Intel and AMD driver bugs will reach players before the project sees them. The 7-day pre-release rule, the driver cohort and benchmark reports, and symbolicated crashes with hardware context limit the damage and shorten report-to-fix time. None of them stops a driver bug from reaching pre-release players first.

## Performance budgets

The simulation holds 60 Hz in the reference battle on the minimum spec, or it degrades in a visible, fixed order. Slowdown is the most common complaint in Stormworks' top-rated negative reviews ([Stormworks negative reviews](https://steamcommunity.com/app/573090/negativereviews/?browsefilter=toprated)), so every number below is a Milestone 0 gate, measured on the rigs below before content work starts. Until a gate first runs, its stated value is the default; each gate names its measurement and its automatic fallback.

### How budgets are measured

The project owns no minimum-spec hardware. Three rigs measure every budget in this document:

- Hosted runners. The free standard GitHub-hosted runners for public repositories: Windows x64 and Linux x64 with 4 CPUs and 16 GB, and macOS arm64 with 3 M1 CPUs and 7 GB ([GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). They measure headless tick time, process memory and, on Windows, Direct3D 12 under WARP for counters that do not depend on GPU speed (pipeline compilations, Godot's `VIDEO_MEM_USED`, main-thread CPU time). They never gate frame rate or GPU time. Shared VMs are noisy, so a timing gate runs 5 times and uses the median of the five p99 (or p95) values. A pull request compares hosted timings only with main's last nightly on the same runner image, and fails if they rise more than 20%.
- The min-spec profile. The owner's Apple Silicon Pro or Max Mac, with a throttle profile that approximates the slowest minimum-spec machine. It measures frame time, GPU time, adapter time, memory, tick time and editor latency. Its rules are below.
- Community benchmark reports. An in-game benchmark scene, run by community testers on their own machines. Reports are the only check on a real Intel Core i5-8400, Ryzen 5 2600, RTX 3050 6 GB or base M1. The rules are below.

Where a gate names both a hosted runner and the profile, both must pass, and the slower result decides.

sim-calib is a fixed CPU calibration run: 600 ticks of the reference battle replay, headless, with the worker pool forced to 1 thread. Its mean wall time per tick is the machine's CPU score. Every rig and every benchmark report records it.

The min-spec profile:

- Threads. The sim pool runs 3 worker threads, as on every machine. Godot's worker pool (`threading/worker_pool/max_threads`) is capped at 2.
- CPU timings are reported in reference milliseconds: measured time × calib_ref ÷ calib_owner. calib_ref is the slower of two class scores. The base M1 score is the median sim-calib of the hosted macOS arm64 (M1) runner over its last 10 nightly runs, replaced by the median of base-M1 community reports once 3 exist. The Windows minimum score is the median of community reports on the i5-8400 and on the Ryzen 5 2600, taking the slower CPU, once each has 3 reports. Until then it defaults to 2.0 × the base M1 score, a deliberately conservative margin. Calibration values live in `perf/calibration.ron` and change only through a pull request.
- GPU. The profile reads the owner Mac's GPU core count C (the "Total Number of Cores" that `system_profiler SPDisplaysDataType` reports). It multiplies the Low tier's 3D scale by s = min(2.0, √(C ÷ 7)), the 7-core M1 being the slowest base M1; 2.0 is the ceiling Godot clamps `scaling_3d_scale` to. It reports base-M1 GPU time = measured GPU time × C ÷ (7 × s²) ÷ 0.9. The 0.9 covers vertex and draw costs that do not scale with pixel count. GPU time comes from an `xctrace` Metal System Trace, or from `MTL_HUD_ENABLED=1` as a cross-check, never from Godot's GPU timer, which is unusable on Metal in 4.7. Gates run with V-Sync off and `max_fps` 0.
- Memory gates run at the Low tier with the s multiplier off, because larger render targets would inflate the footprint.

The benchmark scene ships in every build, from the benchmark pre-release that Milestone 0 spike 5 publishes on GitHub Releases. It is reachable from Main menu > Benchmark and from `--benchmark`. It runs these stages:

1. A 3-minute replay of the reference battle at the detected tier.
2. A 2-minute replay of the co-op reference battle, hosted locally.
3. A 2-minute replay of the minimum swarm.
4. The editor stage (gauge and splash timing, defined in The editor).
5. sim-calib.
6. An optional 20-minute soak of stage 1. It passes when frame-time p95 over its last 5 minutes is 33.3 ms or less on the Low tier (16.7 ms on Medium). A failed soak class falls back like a failed GPU class: the tier's development cut order, one step per release.

It writes a JSON results file: CPU, GPU, driver, OS, physical memory, tier and settings, sim-calib, tick p95 and p99, frame-time p95 and missed vblanks, per-pass GPU times where the API exposes them, peak working set, DXGI CurrentUsage on Windows, phys_footprint, swap growth and the peak memory-pressure level on macOS. It holds no account names, user paths or identifiers. The tester attaches the file to a "Benchmark report" GitHub issue form, and a GitHub Actions workflow parses the form into `perf/reports/`. Nothing is sent automatically.

Report classes and the verification rule:

| Class | Reports that count |
| --- | --- |
| Windows minimum CPU | Intel Core i5-8400 or AMD Ryzen 5 2600, 16 GB |
| Windows minimum GPU | RTX 2060 or RTX 3050 6 GB, 1080p, Medium tier |
| Mac minimum | Base M1 with 8 GB, Low tier; MacBook Air M1 reports also cover the soak |

A class is confirmed for a 0.x minor version when 3 or more reports from distinct machines exist, and it passes when the median report meets every gate in this section. Reports are judged on their stage figures (2 to 3 minutes per battle) against the same thresholds as the 10-minute rig gates. Frame rate is judged as frame-time p95: 16.7 ms for 60 fps, 33.3 ms for 30 fps. A failed class triggers that budget's automatic fallback in the next release. With fewer than 3 reports, the hosted-runner and profile results are the gate of record. The README lists the minimum spec as the target, with the number of confirming reports per class.

### Rates

| System | Rate |
| --- | --- |
| Simulation tick (physics, buoyancy, projectiles, damage) | 60 Hz; 30 Hz for distant AI, meaning hulls more than 10 km from every player-side hull (physics and AI) |
| Ship control graph and officers | 30 Hz |
| Sensor detections | 2 to 10 Hz per sensor, staggered; 5 Hz default |
| Tracks and engagement | 10 Hz |
| Missile and gun guidance | 30 Hz |
| Snapshots to each client | 30 Hz, filled by priority |
| Guided missile state to clients | 5 Hz |
| Client inputs | 30 Hz, each packet repeating the last 4 ticks |
| Distant radar tracks to clients | 1 Hz for tracks more than 10 km from that client's hull; 2 Hz for weapons in flight and tracks closing faster than 30 kn |
| Theatre ledger | One theatre hour per step |

Rate gate (Milestone 0 spike 3, then nightly bot matches on a hosted runner with Gilbert-Elliott burst loss averaging 5%). Three conditions must hold:

- 0.1% or fewer of input ticks are lost.
- The bandwidth gate in Multiplayer holds.
- A distant track symbol jumps no more than 18 px at each update at the default plot scale.

Fallbacks, automatic:

- If more than 0.1% of input ticks are lost, input redundancy rises to 6 ticks.
- If symbols jump more than 18 px, distant tracks go to 2 Hz.
- If a distant hull shows an error of more than 0.5 m when it switches from 30 Hz to 60 Hz, the distant-AI distance becomes 15 km. The 10 km kinematic threshold in the LOD rule stays.
- If the sensor share of the tick fails, the default search-radar rate drops from 5 Hz to 3 Hz. Fire-control radars and seekers keep their rates.

### Budgets

| Budget | Target |
| --- | --- |
| Reference battle | LANTERN plus 40 full-physics hulls (41 hulls), 4 of them enemy capitals of 150 m, the most an operation may field over 90 m; 2,000 live projectiles; 300 persistent hull records in the theatre |
| Co-op reference battle | 4 player hulls against a 25-hull base operation plus 3 task groups of 5: 40 hostile and 44 full-physics hulls, hosted on the Windows minimum spec and on a base M1 |
| Minimum swarm (design floor) | 20 full-physics hostile hulls (up to 4 of them over 90 m), 8 drones (both sides combined), 6 enemy aircraft, up to 24 missiles in flight and 1,000 live projectiles, in a capstone operation on the minimum spec |
| Simulation per 60 Hz tick | 8.0 ms wall time at p99 across a pool of 3 worker cores, on the Windows minimum spec (split below) |
| Godot main thread per frame | 2 ms for the adapter (snapshot read, interpolation, transforms, MultiMesh, mesh and volume uploads; split in Graphics and rendering) |
| Host frame rate | 60 fps on the Windows minimum spec (Medium tier); 30 fps on a base M1 with 8 GB (Low tier) |
| GPU per frame | p95 of 14.5 ms on the RTX 3050 6 GB at Medium; p95 of 24.0 ms on a cold 7-core M1 at Low, which fits 33.3 ms after the 0.72 fanless thermal derate (per-pass budgets in Graphics and rendering) |
| Memory, Windows minimum | Peak process working set of 6 GB or less (of 16 GB); dedicated VRAM of 4.5 GB or less (75% of 6 GB), with the Medium tier targeting 3.5 GB of `VIDEO_MEM_USED` |
| Memory, base M1 8 GB | One combined budget: phys_footprint (CPU and GPU allocations) of 4 GB or less, with the Low tier targeting 1.2 GB of `VIDEO_MEM_USED`; swap growth of 256 MB or less; memory pressure never critical |
| Network | 256 kbps p95 down per client including projectile events, which take 25% or less; adaptive from 128 to 768 kbps; per-vessel replication averages 20 bytes or less per update. A listen host's client limit is floor(0.9 × upload ÷ 256 kbps), so an 8-player host (7 clients) needs about 2.0 Mbps up, inside the 1.5 to 2 Mbps figure |
| Editor | Gauges p95 within 100 ms of an edit; splash p95 under 1 s, 2 s hard limit (gates and fallbacks in The editor) |

The 256 kbps figure is Gaffer on Games' worked example and sits close to the 24 kB/s per client that Stormworks quoted in 2021. At 41 hulls and 30 Hz, 20 bytes per vessel update is about 197 kbps, which leaves about 59 kbps (23%) for projectile events. The network gates and fallbacks are in Multiplayer.

Tick split, wall time per 60 Hz tick on the Windows minimum spec. Lower-rate systems are averaged per tick.

| Subsystem | ms |
| --- | --- |
| Rigid bodies and collider rebuilds | 1.5 |
| Buoyancy and waves | 1.25 |
| Projectiles | 1.0 |
| Damage, flooding, splitting and compartment recompute | 1.0 |
| Sensors (detection, tracks and engagement) | 1.0 |
| Control (graph VM, officers, guidance, Luau) | 1.0 |
| Enemy AI and steering | 0.5 |
| Replication and snapshot encoding | 0.5 |
| Reserve | 0.25 |
| Total | 8.0 |

Tick gate (Milestone 0 spike 2, then nightly on the hosted Windows runner and at each milestone tag under the min-spec profile). A 10-minute reference battle runs headless with per-subsystem tracing spans. Total p99 must be 8.0 ms or less, and the sensor share 1.0 ms or less at p99 under the 300-unit sizing case. The co-op reference battle has the same gate. If the co-op battle misses, its base operation shrinks in steps of 5 hulls until total hostile hulls reach 5 times the player hull count (20 for 4 players), and the task groups stay. Community reports (benchmark stages 1 and 2) recheck both battles on real minimum-spec machines.

Adapter gate (Milestone 0 spike 1). Adapter time per frame (snapshot read, interpolation and uploads) must be p99 of 2.0 ms or less with the reference battle's hull chunk meshes plus 10,000 or more MultiMesh instances at 60 fps. It is measured twice:

- Under the min-spec profile with Metal.
- On the hosted Windows runner with Direct3D 12 under WARP, over the reference battle's peak 60 s segment (3,600 frames at `--fixed-fps 60`, which runs slower than real time). This run times main-thread CPU time rather than wall time, because WARP rasterizes on the same 4 CPUs.

Fallbacks, applied in order until the adapter gate passes:

1. GPU-side interpolation. Transforms upload only when a new snapshot arrives (30 Hz), with previous and current poses packed into the instance buffer and custom data. The vertex shader interpolates from one time uniform, so most frames upload nothing.
2. Per-chunk MultiMeshes with dirty flags. Instances with the rest flag set (wrecks, debris, static props) sit in chunks that are not re-uploaded.
3. An instance cap. Debris and decorative instances beyond 3 km are culled first. Track-bearing objects are never culled.

GPU gate. The min-spec profile gates the base-M1 figure: p95 of 24.0 ms or less in the reference battle replay through smoke and a close splash. Pipeline compilations at draw time (`RENDERING_INFO_PIPELINE_COMPILATIONS_DRAW`) must not rise after load; the hosted Windows runner checks this under WARP, and the profile checks it under Metal. The RTX 3050 6 GB figure is gated only by community reports. Until 3 reports exist, the Medium tier ships with the per-pass budgets in Graphics and rendering, and the first-launch auto-detect flotilla and dynamic resolution protect players at runtime. Automatic fallback: if a GPU class fails its median, the next release applies that tier's development cut order from Graphics and rendering, one step per release, until the class passes.

Memory gate (Milestone 0 spike 5, then every vertical-slice tag). The reference battle runs on two rigs:

- The hosted Windows runner under WARP at Medium, over the peak 60 s segment (3,600 frames at `--fixed-fps 60`). WARP keeps GPU resources in system memory, so the gate is peak working set minus `VIDEO_MEM_USED`, at 6 GB or less. `VIDEO_MEM_USED` itself must be 3.5 GB or less.
- The min-spec profile at Low, for 10 minutes. phys_footprint must be 4 GB or less.

Community reports check the real figures: DXGI CurrentUsage of 4.5 GB or less from QueryVideoMemoryInfo on Windows, and swap growth and memory pressure on 8 GB M1s. Fallback, applied in order on the platform that fails:

1. That platform's low preset halves texture resolution and shadow-map size.
2. Hulls beyond 2 km draw with the lowest generated mesh LOD.
3. The full-physics cap shrinks as below, never under the minimum swarm.

### Degradation ladder

One runtime ladder serves every mode. Overload and connection quality in Multiplayer uses the same ladder:

| Rung | Runs on | Step |
| --- | --- | --- |
| 1 | Each client | Visual detail, in the degrade order in Graphics and rendering (resolution to the tier floor first) |
| 2 | Host | Untracked hulls go kinematic beyond 5 km, then at any range |
| 3 | Host | The projectile cap drops from 2,000 to 1,500 to 1,000; above the cap, one autocannon projectile carries 2 rounds |
| 4 | Host | Time scale drops from 0.9x to 0.5x in steps of 0.1; 0.5x is the floor |

Untracked hulls go before the projectile cap because, by the LOD rule, the player cannot see them. The host's triggers:

- Step up one rung or sub-step when the tick's p95 over a 2 s window exceeds 8.0 ms, then wait at least 4 s before the next step up.
- Step down when p95 stays under 6.0 ms for 10 s.
- Time-scale steps start only after rungs 2 and 3 are spent, and only when the tick backlog exceeds 3 ticks (50 ms behind real time).

The runtime trigger uses p95 over 2 s so that it reacts quickly. The budget gate above uses p99 over 10 minutes. Every rung change shows in the HUD and the log, and every player sees the host's tick rate.

Design-time cap. The full-physics cap is one number for every platform, so co-op and replays match. If either minimum spec misses the 8.0 ms p99 tick in the reference battle at rung 0, the cap shrinks in steps of 5 hulls, never below the minimum swarm.

Before the floor is touched, the ladder's rungs 1 to 3 go first. The minimum spec does not rise.

Swarm gate (Milestone 0 spike 5, re-run at the vertical slice and each act). The minimum swarm must meet the 8.0 ms p99 tick on the hosted Windows runner and under the min-spec profile, with community reports as confirmation. It may use rung 3 or below, never rung 4. Host frame rate holds at 30 fps on the M1 (base-M1 GPU time p95 of 24.0 ms or less under the profile) and at 60 fps on Windows (frame-time p95 of 16.7 ms or less in community reports of stage 3). A frame-rate miss falls back like a failed GPU class. The floor itself has no fallback. If the swarm still misses after rung 3, these give way in order, applied automatically when the gate reports:

1. Full-physics enemy hulls tick at 30 Hz, the existing distant-AI rate. Player-side hulls stay at 60 Hz.
2. The whole simulation ticks at 30 Hz with a 16 ms budget per tick, and the client interpolates.
3. Capstone operations replace their hulls over 90 m with classes of 90 m or less. If that still misses, enemy class length in capstones is capped at 60 m, which reduces cells per hull while the count stays at 20 or more. This removes the 150 m capitals from capstones and is the last resort.

### Minimum spec

| Platform | Minimum | Basis |
| --- | --- | --- |
| Windows | Intel Core i5-8400 (6 cores, 6 threads, Q4 2017) or AMD Ryzen 5 2600 (6 cores, 12 threads, 2018), 16 GB RAM, 6 GB VRAM (RTX 2060 or RTX 3050 6 GB class; GPU budgets are set against the RTX 3050 6 GB, which has half the 2060's memory bandwidth), 1080p | 83.7% of Steam users have 6 GB VRAM or more; 6- and 8-core CPUs are 56.0%; 1080p is 50.23%. Both CPUs are the oldest 6-core parts on Microsoft's Windows 11 supported-processor lists, and both exceed Godot 4.7's SSE4.2 floor |
| macOS | Base M1 with 8 GB, macOS 14 or later, Low tier | M1 is 14.22% of Steam Macs and 8 GB is 28.77% |

The requirements line in the README and the GitHub release notes reads "Intel Core i5-8400 or AMD Ryzen 5 2600"; a Steam store page, if one ever exists, uses the same line. The i5-8400 is the reference machine for the 8 ms tick, the 2 ms adapter budget and 60 fps. Without SMT, its 6 threads are the tightest fit for 3 sim workers plus the Godot main and render threads. The Ryzen 5 2600 is held to the same budgets: the calibration takes the slower of the two. Builds do not require AVX2, though both CPUs support it.

### Level of detail by relevance

Detail follows what the player can affect, not a fixed radius. Any unit the player can track, target or is targeted by gets full sensor, projectile and damage simulation, so the player's long-range shots never resolve as dice rolls.

One rule decides the tier. A hull gets full physics if any of these holds, whatever its range:

- Any player-side sensor tracks it.
- Any player-side weapon targets it.
- It targets a player-side unit.
- A projectile will reach its bounding box within the next tick.

Untracked hulls within 10 km of a player hull also get full physics, until rung 2 of the ladder moves them to kinematic motion. Every other hull beyond 10 km of all player hulls moves kinematically with analytic buoyancy. Full-physics hulls beyond 10 km tick at the 30 Hz distant-AI rate. Abstract combat is reserved for fights between enemy units that no player-side sensor observes.

Physics engines thread by islands of touching bodies, and a ship is one island, so island threading does little for one large ship. Black Pennant parallelises inside each vessel instead: buoyancy per chunk, conduits, flooding and graph evaluation run as separate jobs.

A per-vessel complexity budget appears in the editor as a gauge, so players see the cost of a design before a fight slows down. It is measured in complexity points, about 1 single-core microsecond per 60 Hz tick on the minimum spec each. The gauge shows Caution above 400 points. In the campaign it is information, not a cap. PvP caps are 600 points per vessel and 3,600 per lobby, about 1.2 ms of wall time on 3 cores. The definition and fitting rules are in Multiplayer.

## Interface and visual design

These rules cover the game UI, the editor, the GitHub README, the GitHub Pages documentation site (static HTML built in CI from repo markdown; no other website), release notes, and the Steam store page if Steam happens. They come from a second Opus research round (seven verified topics) on generic AI-looking UI and what consoles, naval sims and accessibility guidelines require, and override self-contradicting vendor anti-slop advice (100/200 against 800/900 weights, staggered reveals).

### Visual direction

The UI is a near-future frigate CIC and design office console, grounded in IHO ECDIS colour rules and high-performance HMI. No CRT emulation, scanlines, phosphor bloom, holograms, glass panels or fake telemetry.

- Palette. Normal is grey: blue-grey surfaces, never pure black, no brand accent (near-black plus acid accent marks generated UI). Hue only codes. The chart table is the one light surface, in exact NOAA chart colours, not a cream editorial preset.
- Type. One sans family for small-size legibility with tabular figures, Regular and Bold. Sentence case; capitals belong to codes.
- Shape. Radius 0 for frames, gauges and panels, 2 px controls, 4 px tiles and graph nodes; lint fails above 4. Opaque docked panels; structure from alignment, spacing and 1 px rules; cards only for selectable things.
- Motion reports state: only real data moves (contacts, needles, a sweep at the simulated rate); a change gets a short ease-out or box highlight.

### Eyebrow text

The most recognisable tell of generated UI now: any short label directly before a larger heading (eyebrow, kicker, overline, tagline, pretitle), in caps, tracked, smaller, muted or a pill.

- Anthropic's frontend-design skill lists tracked all-caps eyebrows, middle-dot meta strings, "WORD — fragment" labels and decorative monospace as generic defaults ([frontend-design skill](https://raw.githubusercontent.com/anthropics/skills/main/skills/frontend-design/SKILL.md)).
- Impeccable's May 2026 lint flags a 2 to 30 character tracked uppercase sibling before a large h1 ([Impeccable PR 129](https://github.com/pbakaus/impeccable/pull/129)).
- 22% of 1,590 Show HN sites matched four or more slop patterns, hero badge included ([Krebs](https://www.adriankrebs.ch/blog/design-slop/)).
- HN readers call "the small all-caps eyebrow" a Claude-ism ([HN](https://news.ycombinator.com/item?id=49765151)) and flag two-digit section numbers on game sites ([HN](https://news.ycombinator.com/item?id=50068311)).
- It descends from Material's 2018 overline (12 px uppercase), now M3 labelSmall ([M2 to M3 mapping](https://developer.android.com/develop/ui/compose/designsystems/material2-material3)), and fails the 18 px floor.

Ruling: banned on the README, docs, release notes, Steam assets, trailers and every menu and settings screen. On game screens the one exception is a meta line bound to data the heading lacks (track or hull number, date-time group, "Mission 3 of 7"). It passes Cover (hidden, the heading still reads) and Swap (moved, it is wrong), uses Meta (body size, no tracking), sits after or beside the heading, at most ceil(panels / 4) per screen. Nothing is named Eyebrow, Kicker, Overline, Tagline, SectionLabel or Pretitle. Godot lint: a Label before a title Label; a Meta Label unbound to res://ui/lint/meta\_fields.txt; excess Meta lines. Web lint: 16 px or less, uppercase or tracked over 0.03em, directly before h1 to h3.

### Banned patterns

One match is noise; many are a signature. Web is README, docs and release notes; Store is Steam only; blank means none.

| Pattern | Scope | Instead | Check | Exception |
| --- | --- | --- | --- | --- |
| Eyebrow chip or pill over a hero or menu heading | Web, store, menus | Facts in the heading; news in release notes | Web lint; menu screenshots | Steam event text by Artwork Override, 14 days |
| Kicker restating a panel title ("PROPULSION" over "Engines") | Game UI | One title; bound Meta line | Godot lint | Bound meta line |
| Sentence split across label and heading | Copy | One heading | Cover the label | |
| Label above and badge below one title | Game UI, web | One support line max | Over one non-heading Label by a heading | |
| Two-digit numbers on non-sequences ("01 / Features") | Game UI, web, copy | Plain headings | Regex (^\|\\s)0\[1-9\]\\s\*\[/.\\-–\]\\s, skipping dates and bearings; hits need an ordered-content note | Tutorial, briefing, waypoint and mission order |
| Dot prefixes, hairlines, pulsing "LIVE" dots, blinking badges | Game UI, web | Marks bound to state | Status dots reference a sim signal; grep bullets | Live indicators (radar emitting, link lost) |
| "SYS://SONAR", hex, fake telemetry, decorative brackets, ticks, grids | Game UI | Real values or space | Grep //, 0x, ::; each glyph names a sim value | Real designations; labelled rings |
| Gradient text; purple, indigo or cyan glow; coloured shadows; acid accent | Game UI, web | Token colours, grey normal | Grep background-clip: text, bg-clip-text; Label shader lint; Color literals outside res://ui/theme/tokens.json; shadow\_color saturation above 0 | Logotype |
| Glassmorphism | Game UI, web | Opaque docked panels | Grep BackBufferCopy, blur shaders, backdrop-filter | Pause or radial scrim, opaque under reduce transparency |
| One radius and shadow everywhere, nested cards, icon-tile grids | Game UI, web | Rules, spacing, tables | Shadowed PanelContainer in another; two or more radii per screen | Part tiles, graph nodes, save slots |
| Coloured side or top stripes on rows | Game UI, web | Spacing and type | Single-side borders only in Severity, Stance, Damage | Those three |
| Hero metric tiles; bare gauge numbers | Game UI | Value against band and limits | Gauge checklist: band, limits, trend or waiver | Exact readouts with units |
| Stat strips, invented numbers, anonymous testimonials, logo clouds | Web, store, copy | Linked quotes, real footage | Repo source link per number and quote | Steam review widget; dated figures |
| Inter, Roboto, Godot's Open Sans, Space Grotesk, Geist, Instrument Serif; italic serif accents | Game UI, web | Fonts in fonts.md | CI fails unlisted font files | |
| Decorative monospace | Game UI, web | Proportional tnum | Mono only in Expression, Traffic | Node-graph code, message traffic |
| Unthemed Godot defaults; GraphEdit menu, minimap, grid chrome | Game UI | Project theme | Default-theme diff; GraphEdit lint | Debug overlays, stripped |
| Entrance fades, staggered reveals, bounce easing, pulsing, marquees, count-ups | Game UI, web | Instant content | Tween lint (implementation 11); diff a 30 s calm capture | Sim motion, camera moves |
| Blinking text; rates other than 1 and 2.5 Hz | Game UI | One symbol, 2.5 Hz max | No looped visibility or modulate on Label; cycle 400 ms or more | |
| Status by hue alone (green running, red stopped) | Game UI, editor | Shape, glyph or word too | Name states from greyscale and deuteranopia captures | |
| Toast per event; timed alarm toasts | Game UI | Inline state, log, acknowledged alarms | Toast check below | Global messages ("Design saved") |
| Officer sparkle or "AI" badges; emoji; Lucide or Heroicons; mixed icon styles | Game UI, web | Billeted officers; one hand-written SVG set, 24 px grid, 2 px strokes | Emoji regex; icons only from res://ui/icons | Player-typed text |
| Uppercase multi-word text; capitals in source | Game UI, copy | Sentence case; Code for codes | Label.uppercase false; all-caps word pairs not in the glossary | |
| Copy tics (copy rule 9), puffery, button arrows | Copy | Checkable operator language | banned\_phrases.txt; sentence-length lint; title-case regex | Radio chatter; save rows; directional arrows |
| Tooltip restating its label ("Fire control: controls firing") | Copy | Effect with units | Label plus under 4 words fails | Icon-only: name plus shortcut |
| Capsule beyond render, name and subtitle; other lettering; HUD shots; generated sheen | Store | In-engine render of saved designs; logotype in Atkinson Hyperlegible Next Bold | View at 120x45; manifest names scene, blueprints, font | Artwork Override, 14 days |
| Concept art, offline renders or marketing text as screenshots | Web, store | In-engine captures with UI; first four show the core loop | Path in the capture folder | |
| Text baked into rasters; pseudo-text on hulls, flags, gauges | Art | In-engine text | Reviewer name and date after a 100% zoom check | Logotype; illegible wear |
| Generative image, audio, voice or music output; unquarantined placeholders | Art, audio | Code-made assets; magenta checker or grey box with TODO ID | Release CI: no res://\_placeholder/ reference; provenance row names the making script | |
| Ships not from game vehicles; studio pastiche | Art | Renders of saved designs | Each pictured ship is a save file | |

Toast check (scripted 20-minute battle, each UI change): fails on any sim-event toast, over 1 per 60 s or 3 per battle. Only save, join or leave, and Steam achievements toast. Fallback: co-op join and leave move to the log, leaving "Design saved" and connection loss.

Sentence-length lint (web and store copy): fails if 5 consecutive sentences have a length coefficient of variation under 0.40, or 4 consecutive 8 to 20 word sentences sit within 5 words. Calibration before the first public release: flag 80% of 20 agent blurbs, at most 10% of 20 human store blurbs (kept out of the repo). Fallback: the threshold moves in 0.05 steps within 0.25 to 0.55; if none passes, CV only warns.

### Tokens

Dusk (console, editor) and chart ship by the vertical slice (M1), high contrast by the first public release. Day and night console palettes are cut; over 3D day and night presets the HUD keeps dusk or high contrast, checked by HUD rule 18. Ratios are WCAG on surface.raised, truncated.

| Token | Dusk | Use | Ratio | High contrast | Ratio |
| --- | --- | --- | --- | --- | --- |
| surface.base | #1C2328 | Panels, HUD backing | | #0F1316 | |
| surface.raised | #262E34 | Raised panels, popups | | #161B1F | |
| surface.inset | #151A1E | Wells, plot, badge text | 5.25 on warning, 9.58 on caution | #090C0E | 11.11 on caution |
| text.primary | #E4E8EB | Body, values, track labels | 11.19 | #FFFFFF | 17.34 |
| text.secondary | #B2BAC1 | Support, units, rule.reference | 7.01 | #DCE1E5 | 13.17 |
| text.inactive | #87919A | Disabled only | 4.29 (XAG 3:1) | #AEB7BF | 8.52 |
| rule.divider | #4A545C | Non-informational rules | 1.78 | #8C969E | 5.75 |
| stance.friend | #80D8FF | Frames only | 8.65 | #99E2FF | 12.12 |
| stance.hostile | #FF8A80 | Frames only | 6.04 | #FFA8A0 | 9.38 |
| stance.neutral | #A8F0A8 | Frames only | 10.30 | #B8F5B8 | 13.86 |
| stance.suspect | #FFF080 | Frames; default stance | 11.83 | #FFF59E | 15.53 |
| alert.caution | #FFB020 | Caution badges, strip | 7.54 | #FFB52E | 9.83 |
| alert.warning | #FF4A3D | Warning badges, strip, editor Fail, blocked arcs; never text (text.primary on it 2.70) | 4.13 as shape | #B00000 | Fill 2.35, so a mandatory 2 px text.primary border (SC 1.4.11); text.primary on it 7.37 |
| select | #FFFFFF | Selection bracket | 13.79 | #FFFFFF | 17.34 |

Stance tints are house values in conventional hue families, not standard codes, used only in frames and the Stance stripe. ui.focus is #FFFFFF at 2 px; ui.box is text.primary at 2 px; rule.reference (graticules, rings, bearing lines, scale bars) is text.secondary. High contrast hatches blocked arcs in text.primary and keeps warning a deep red unlike the hostile tint.

Chart tokens are exact palette entries of public-domain NOAA Chart 18649, Entrance to San Francisco Bay, last edition ([PDF](https://historicalcharts.noaa.gov/pdfs/18649.pdf), [terms](https://nauticalcharts.noaa.gov/learn/faq.html)), credited to NOAA Office of Coast Survey, meanings per [U.S. Chart No. 1](https://nauticalcharts.noaa.gov/publications/us-chart-1.html): water.deep #FFFFFF (base), water.shallow #D8F0F5, land #EBE2C8, land.builtup #D9CA96, foreshore #C1D1AF, ink #000000 (all text, graticules; 12.83 on built-up), overlay #953699 (routes, lines only; 3.93), overlay.text #950096 (magenta labels; 4.72), overlay.tint #A06EA0 (zones, on water and plain land only, with pattern or label; 3.10). Only ink and overlay.text carry text (ink alone in high contrast).

Type is Atkinson Hyperlegible Next v2.001 (Braille Institute, OFL 1.1, no Reserved Font Name) at wght 400 and 700 via `variation_opentype`, tnum digits 632 units. Expression and Traffic use Atkinson Hyperlegible Mono v2.001, set to wght 400 or 700 (default 200). B612 lost: 5 of 128 Latin Extended-A, no OpenType features. Atkinson is Latin only (93 of 128 Extended-A, no µ); locales needing more add OFL fallbacks Noto Sans (Cyrillic), Noto Sans SC or Noto Sans JP, digits staying Atkinson. CI fails on string characters missing from the locale's font chain (minutes use an ASCII apostrophe; arrows are icons). The docs site self-hosts both as WOFF2; OFL.txt ships in res://ui/fonts/licenses/.

- Scale as rendered body height at 1080p, font\_size from body ratios 0.910 Next and 0.902 Mono: 18 px (20) Dense, PlotLabel, Code; 20 px (22) Body, Meta; 24 px (27) PanelTitle; 30 px (33) ScreenTitle; 40 px (44) MenuTitle. Line height 1.5 over two lines, 80 characters max.
- Spacing 4, 8, 12, 16, 24, 32, 48. Strokes: 1 px rules; 2 px frames and icons on a 24 px grid; 2 px focus; 3 px selection.
- Motion: 0 ms default; 100 ms feedback; 150 ms panel change; 250 ms ceiling outside camera moves; ease-out. Blink 1 and 2.5 Hz. Mode-change box 10 s.
- Symbols, flat 2D, original, no NATO or NTDS shapes: Friend circle, Neutral 2:1 capsule, Suspect downward shield (pentagon), Hostile upward triangle; air a 2 px bar above, subsurface below. Gate each milestone: 10 reviewers name stance and domain of 24 symbols at 18 px in greyscale and deuteranopia, 95% correct; failures are redrawn in family.
- Damage, never hue: Intact (over 75%) clear; Damaged (40 to 75%) 45° 1 px hatch at 6 px, text.secondary; Heavy (1 to 39%) at 3 px, text.primary; Destroyed 45° and 135° at 3 px on surface.inset. Flooding: horizontal lines at 4 px. Gate: 10 reviewers name levels of 12 parts at 18 px in greyscale and deuteranopia, 90% correct. Fallback: Damaged becomes a 2 px dot grid and hover shows health percent.

### Sortie HUD rules

Bridge view and plot table only.

1. Quiet when normal: zero alert-colour pixels (exact match), no blinking or loops.
2. Six coding hues. Selection is a white bracket, damage is value and hatching, Ship's Notes severity is glyph, word and alert hue.
3. Hostile keeps the genre red tint; warning red appears only in badges and the strip, always with sound, never on tracks.
4. Stance by frame, tint and label, domain by modifier; the plot reads in greyscale and under Brettel or Viénot simulation; labels use text.primary.
5. Track numbers are never reused; merges, splits and renumbering show on symbol and log. Climb, dive or steady is a symbol glyph.
6. Uncertainty keeps sensor shapes (bearing-only wedge, fire-control circle) with graded edges, lighter value and sample points, never hue, plus a legend; hard edges only for real boundaries.
7. ID rung as word (Unknown, Domain, Type, Class, Exact) plus zero to four bars; stance as word plus frame; changes get the box and a log entry.
8. The docked Bridge Board shows per officer task, evidence-citing reason ("Holding fire: Suspect track, not inside self-defence range"), next action, control state (Officer, Player or Advisory) and override count with one-click revert, plus radiating emitters and ESM warning. No spinners or "Analyzing".
9. Each role (helm, weapons groups, sensors, engineering) shows its control state; mode changes, takeovers and releases get the box and a log entry; hold-to-aim shows a hand glyph.
10. At most 20 new alerts per 20-minute engagement; over 10 in 10 minutes is a flood (summarise, suppress lower priorities); about 12 an hour in theatre time. The scripted battle fails above either.
11. Blink only symbol-sized marks, fast on one item, stopping on acknowledge; below MIL-STD-1477C's 4 to 5 Hz to meet WCAG 2.3.1.
12. Gauges show value with units against band and limits, trend where rate matters; the value has highest contrast.
13. Every line is a labelled reference (graticule, ring, bearing line, scale bar); no decorative lines, no 3D models as icons.
14. Overview first: sparse default, density from player layers, text 18 px or more at 100% plot scale. Overlapping labels hide lowest first, ranked own hull and selected track, inbound weapons, Hostile in own weapon range (by closest approach), Hostile outside, Suspect, Unknown rung, Neutral and Friend, ring labels, waypoints; hover shows them.
15. The log keeps full history, filters by type and unit, marks unread, centres on click, and takes routine events silently.
16. Tab switches bridge view and plot table; no continuous zoom.
17. Every station and delegation is three actions from the plot; diegetic controls have shortcuts.
18. HUD text over 3D has an outline and opaque backing (default 100%), adjustable down to the lowest opacity giving 4.5:1 over the worst spray, sky, night or fire sampled in any 3D preset.

### Alert priorities

Four [MSC.302(87)](https://wwwcdn.imo.org/localresources/en/KnowledgeCentre/IndexofIMOResolutions/MSCResolutions/MSC.302(87).pdf) priorities, each with its own glyph, word and synthesized sound.

| Priority | Use | Badge | Unacknowledged | Sound | Acknowledge |
| --- | --- | --- | --- | --- | --- |
| Emergency | Ship loss imminent | alert.warning | Blinks | Continuous | Required, no timer |
| Alarm | Act now: weapon inbound, flooding, fire | alert.warning | Blinks | Repeats until ack or rectified | Required, no timer |
| Warning | May become hazardous | alert.caution | 1 Hz | Chime, again at 120 s | Required |
| Caution | Out of the ordinary | alert.caution | Steady | One tick | None; self-clears |

Acknowledged alerts hold steady until rectified; only the newest top Emergency or Alarm blinks fast; blink-off shows a 3 px outline. Silence all lasts 30 s. Acknowledge takes the highest, oldest alert; no acknowledge-all; only the player acknowledges. Rectified unacknowledged Emergencies and Alarms show "Cleared"; lower ones go to the log unread. Warnings escalate by threshold, never timer. Fire or Flooding Alarms absorb same-compartment Warnings and Cautions; an Emergency suppresses both; both shelve for 5 minutes or the sortie. Floods send Cautions to the log and combine Warnings per subsystem; Emergencies and Alarms never combine. Only first annunciations count; mix target 80% Caution, 15% Warning, 5% Alarm, under 1% Emergency (ISA-18.2). The strip shows five plus "+N"; the log keeps all times.

### Editor and chart colours

Editor states pair glyph and word: Pass (text.primary; check or in-band value), Caution (triangle on alert.caution), Fail (cross on alert.warning), badge text in surface.inset. Green is never pass; it is Neutral. Pins and overrides get a pin or hand glyph. Editor overlays (conduits, vulnerability heatmap, arcs, blind sectors) and chart layers are categorical, exempt from the hue cap, with legends and pattern or label separation; blocked arcs hatch in alert.warning. Hull paint is Graphics and rendering's naval swatches plus a clamped picker.

### Copy rules

1. Sentence case everywhere. Capitals only for glossary entries (acronyms, track numbers, hull designators, classification, precedence); stance, rung and billet words keep an initial capital.
2. Panel titles name the instrument ("Radar", "Fire control").
3. Buttons are effect verbs ("Launch decoys"); no non-directional arrows, "Get started" or exclamation marks.
4. Reasons give cause and evidence under 60 characters: "Turning 040: torpedo bearing 210, closing."
5. Ship's Notes name part, cause, value, limit and fix, linking the part (schema lint).
6. A tooltip's first line is 12 words or fewer: effect with units or the condition changing it.
7. Merge labels into values ("3 contacts", "12 kn"); label-over-value only on spec sheets.
8. Banned words: introducing, unlock, empower, elevate, streamline, transform, discover, seamless, smart, next-gen, revolutionize, leverage, delve, vibrant, pivotal, showcase, tapestry, testament, unleash, undated "new"; refreshed quarterly from Wikipedia's Signs of AI writing.
9. Banned: "not just X, it's Y", rule-of-three slogans, staccato headlines ("Build. Fight. Command."), uniform 8 to 20 word runs, Title Case, emoji, middle-dot taglines, "WORD — fragment", over one dash per paragraph.
10. Briefings use orders format (situation, mission, execution) with date-time groups, designators and numbers; no marketing language in the game.
11. README, docs and store copy name the genre, mechanics as verbs and the hook; no invented stats or testimonials (the FTC's 2024 fake-review rule).
12. Officers and the seven aces are text, never voiced, with radio sounds (squelch, static, band-pass): two 40-character lines, speaker named at changes, shown no later than the sound.
13. Strings go in tr() with a context column from day one, in normal case; codes as res://ui/lint/glossary.txt spells them. English first; contributors translate by pull request or Hosted Weblate's free libre plan, aiming at Simplified Chinese, Russian, Spanish, Brazilian Portuguese, German, Japanese and French for 1.0.
14. AI disclosure (README from the first public release; Steam's [Content Survey](https://partner.steamgames.com/doc/gettingstarted/contentsurvey?l=english) if Steam happens): code, text and procedural assets were written by an AI coding agent under owner direction, and no generative image, audio, voice or music models were used. On Steam, shipped and store text is Pre-Generated; nothing is Live-Generated.

### Accessibility floors

1. Text body height 18 px at 1080p (36 px at 4K), plot labels included, per [XAG 101](https://learn.microsoft.com/en-us/gaming/accessibility/xbox-accessibility-guidelines/101).
2. UI scale 75% to 200%, no clipping at 200% (one-axis scroll, abbreviation allowed).
3. Contrast 4.5:1 for text; 3:1 for 36 px text (logical px at 100%), frames, icons, focus and inactive text (XAG 102, stricter than WCAG); 7:1 in high contrast.
4. Never colour alone. Stance and alert recolouring is held to item 3.
5. Targets 24x24 logical px; focus 2 px, 3:1 between states; actions fire on release or undo.
6. At most three flashes a second; a 3D flash counts if its bloom exceeds 2.5% of the screen, extras drawing at 25% emission. Reduce flashing caps VFX emission at 4 and removes lightning.
7. Reduce motion (from the OS, which also sets blink-off) uses 150 ms fades or instant change, never no feedback. Sliders 0 to 100% for shake, blur (default 0; no motion blur in v1) and camera roll.
8. Everything rebindable per device, Esc included; holds toggle; no chords or mashing; non-drag wiring and placement.
9. Blocks over two lines are start-aligned, 80 characters, line height 1.5.
10. Officer and ace lines scale to 200%, background opacity 0 to 100%, with a preview.
11. Icon-only controls have an accessibility\_name; no emoji or Unicode as functional icons.
12. Screen readers reach every track via plot sub-elements and the track table, a Tree sortable by range and threat, one action from the plot.

### Godot implementation

1. `res://ui/theme/tokens.json` (colours per palette, sizes, font ratios) compiles by `@tool` script into `project_theme.tres`, set as `gui/theme/custom`; citations sit in `theme_tokens.md`, as .tres drops comments.
2. No `theme_override_colors`, `_fonts`, `_font_sizes` or `_styles` in .tscn, nor literal `add_theme_*_override()`; separation and margin constants may use tokens.
3. Looks are type variations only: ScreenTitle, PanelTitle, MenuTitle, Body, Dense, Meta, Code (the one small label), PlotLabel, Expression, Traffic, Severity, Stance, Damage; none named like a base type.
4. `Label.uppercase` is false. Code sets `spacing_glyph` 1 px and tnum and matches glossary.txt.
5. Numerals use tnum in `opentype_features` (key `TextServerManager.get_primary_interface().name_to_tag("tnum")`, value 1); track and hull numbers are tabular, not mono.
6. Stretch `canvas_items`, aspect `expand`, scale mode `fractional`, base 1920x1080, `allow_hidpi`, `use_oversampling` and `snap_controls_to_pixels` true. UI scale drives `get_tree().root.content_scale_factor`, never times OS DPI.
7. Fonts import Grayscale, Light hinting, no MSDF; zoomable plot and chart labels use MSDF with `msdf_pixel_range` at least twice the outline.
8. `font_size` derives from body-height tokens and ratios; a screenshot test re-measures.
9. Plot and gauges are `_draw()` nodes reading theme colours, constants and fonts from custom types, `queue_redraw()` on data change only, `draw_multiline()` for ticks, `draw_polyline()` for track history.
10. GraphEdit turns off menu, minimap, zoom, grid, arrange and minimap buttons and zoom label, keeps a themed grid; a model layer owns connections.
11. No `TRANS_ELASTIC`, `_BOUNCE`, `_BACK` or `_SPRING`; non-linear Tweens use `EASE_OUT` and token durations, and animate `offset_transform_*` (visual only), never position, size or anchors. Each Tween or AnimationPlayer names its sim signal in metadata.
12. Controls have themed focus StyleBoxes, explicit neighbours and `grab_focus.call_deferred()`. Gameplay never binds `ui_*`, reads keys in `_unhandled_input()`, and removes Tab and Space from `ui_focus_next`, `ui_select` and `ui_accept`, so no Control swallows the view switch or Splash.
13. First launch reads `DisplayServer.accessibility_should_reduce_animation()`, `_reduce_transparency()` and `_increase_contrast()`; 1 sets Reduce motion plus blink-off, reduce transparency, or high contrast. Major panels set `accessibility_region` and `accessibility_name`.
14. QA builds pseudolocalise with `expansion_ratio = 0.3`, `fake_bidi = true` and `replace_with_accents = false` (Atkinson lacks 20 accent targets); the harness renders "ÅÉÎÕÜ Çgjpqy ŁŚŻĆŃ ĞŞİı" at every step. Numbers use `TranslationServer.format_number()` (`TextServer.format_number` is deprecated in 4.7). No fixed-width text containers.
15. Temporary art lives only in `res://_placeholder/`, excluded from exports.
16. CI fails if any shipped Control type's `ThemeDB.get_default_theme()` item is missing from the project theme.
17. Like ItemList, the plot is ROLE\_LIST\_BOX with one `AccessibilityServer.create_sub_element` option per track (tr() name such as "Track 0412, Hostile, bearing 047", bounds, selection, focus, click), rebuilt on `NOTIFICATION_ACCESSIBILITY_UPDATE`, moved at 1 Hz; `_get_focused_accessibility_element()` returns the selection. The alert strip is a live region. Rust implements it via gdext 0.5.5. Gate each milestone: VoiceOver on the owner's Mac reads every track; a UI Automation script on the hosted Windows runner finds one named item per track. Fallback: the plot's name points to the track table.
18. Windows exports carry a PerMonitorV2 DPI manifest. Gate: each PR's export reports that awareness on the hosted Windows runner, and the benchmark scene logs it for testers. Fallback on failure or reported blur: an engine patch (awareness before window creation, resize on WM\_DPICHANGED) in the custom export templates.

### UI review process

1. AGENTS.md and CLAUDE.md carry this spec. Never prompt "be distinctive": taste prompts raised tell counts.
2. CI on UI changes runs every lint above plus docs-site DOM checks.
3. A harness captures each screen with real data at 1x and 2x in dusk and high contrast (chart screens in chart) on hosted macOS and Windows (WARP) runners; EA's [Fonttik](https://github.com/electronicarts/fonttik) checks text.
4. Before handoff the agent attaches its scored captures to the PR. One hit needs a written argument; two on a screen fail.
5. Per screen: does covering a label drop meaning? Is every colour nameable, every glyph a sim value, every gauge value brightest?
6. From greyscale and colour-blind captures the reviewer names every stance and alert priority; a playtester says from a still what each officer does next and why.
7. A 30 s calm capture shows no alert pixels or untraced change; the scripted battle logs alerts and toasts.
8. Each milestone and release runs EA's IRIS analyser on combat captures and trailers and checks provenance, zooming rasters to 100%.
9. Each milestone, add new recurring defaults from the last 20 agent screens to the ban list, dated. Tells drift: the generated-UI accent moved from indigo to emerald.

### UI playtest gates

Testers are the owner and GitHub Discussions volunteers; a gate short of its sample ships its default and re-runs next milestone.

- Uncertainty (vertical slice, 10 testers, 6 stills): fails if over 50% read a shape as ship size. Fallback 1: 2 px sample points only, under a fixed-size symbol. Fallback 2: ellipses and circles only on hover and selection.
- Alerts (vertical slice, 10 testers): 80% answered in 10 s, at most 5% of Emergencies and Alarms unanswered at 30 s. Fallback: ceiling 15, flood at 8, lower priorities to the log; under 50% answered makes Caution log-only.
- Declutter (vertical slice): reference-battle captures (41 hulls plus missiles) show the top three priorities unoverlapped; testers find a named track in 5 s median. Fallback, never below 18 px: lower labels shrink to track numbers, then leader lines.
- Codes (vertical slice, 20 naval-sim players): A/B stills rated authentic, costume or no opinion. Capitals stay unless rated costume 20 points more than mixed case; then codes become words ("Bearing 040") except track numbers, hull designators and briefing markings.
- Captures (before the first public release and any store upload, 30 viewers): the HUD never changes. If over 50% call calm shots dull, 3 of the first 5 show crisis and calm shots move past 5.

## Graphics and rendering

Ships stay honest 0.5 m cells; beauty comes from light, haze, cell-derived shading, a sim-matched sea and restrained post, all made in code. Nothing is profiled yet; Verification gives each millisecond budget's gate and fallback.

### Art direction

The look is welded-steel geometry in naval greys, lit like a photograph at day's end. It departs from Stormworks' flat-lit coloured blocks through tonal paint, light-derived detail and atmosphere, never smoother geometry. The levers, in priority order, work on every renderer:

1. Light: a low warm sun, a cool sky fill, rim light, and glints on cell edges ([Albrecht](https://www.gamedeveloper.com/design/how-to-make-low-poly-look-good)).
2. Depth layers: a crisp near field, hazed mid-range silhouettes, and a far band of islands, cloud masses and fronts. Fog is "really, really key" for flat-shaded depth ([Sable](https://www.gamedeveloper.com/marketing/how-shedworks-refined-the-art-of-sable-in-pursuit-of-readability)).
3. Cell-derived AO, bevels, plating seams and weathering. No textures.
4. Value before hue: hull swatches use US Navy WWII paint values, since BuShips found "tone was far more important than hue" ([USN](https://en.wikipedia.org/wiki/World_War_II_ship_camouflage_measures_of_the_United_States_Navy)).
5. Brightness goes to gameplay. Wakes and VFX are the brightest pixels, and only emission and sun glints bloom.

Dusk is the hero light (skirmish default, promotional captures). Ships have no outlines or edge lines on any tier; separation comes from rim light, fog layers, value contrast and wakes.

Rule A0: a capture with SSAO, glow, SSR and volumetric fog off still passes art review.

Pixel math at 1080p, 60° vertical FOV: 1 m at d metres covers 935/d px. A 0.5 m cell is 4 px at 117 m and 1 px at 467 m; a 0.04 m bevel 2 px at 19 m; a 90 m hull 42 px at 2 km. Runtime code uses the live FOV.

Swatches live in `res://art/tokens/world_tokens.json` with linear Y, OKLCH, sRGB and Munsell and paint citations; hue 5PB or neutral, OKLCH C ≤ 0.06 (the Munsell chroma 4 limit), converted by a Rust colour library and committed. Table sRGB values are neutral approximations. LUTs, ramps and noise come from the tokens via Rust build steps, never a grading tool or model.

| Token | USN paint | Y | sRGB | L* |
| --- | --- | --- | --- | --- |
| hull.pale | 5-P Pale Gray | 0.55 | 196 | 79 |
| hull.light | 5-L Light Gray | 0.35 | 160 | 66 |
| hull.haze | 5-H Haze Gray | 0.28 | 144 | 60 |
| hull.ocean | 5-O Ocean Gray | 0.17 | 115 | 48 |
| hull.sea | 5-S Sea Blue | 0.11 | 93 | 40 |
| hull.navy | 5-N Navy Blue | 0.09 | 85 | 36 |
| hull.deck | 20-B Deck Blue | 0.05 | 63 | 27 |
| hull.black | Dull Black | 0.03 | 48 | 20 |

Decks default 3 or more steps darker than sides. The editor adds a 1-cell hull.black boot-topping band at the design waterline, with red-brown antifouling (Y 0.08) below. Hull numbers are in-engine MSDF text.

Paint: a blueprint palette of at most 16 entries (the volume's 4-bit swatch field) holds the 8 swatches, antifouling and up to 7 picked colours. Hull albedo clamps in OKLCH, chroma first, to Y ≤ 0.6 and C ≤ 0.06 (markings Y 0.7, C 0.12) on pick and load.

The owner runs G1 and G3 each milestone; scripts on the goldens run G2, G4 and G5. A failure blocks sign-off and is fixed in content (G1 kit profiles, G2 horizon fog 5 L* per pass, G3 a 2-cell band, G4 seams at 4%, G5 a seeded front), never a lower threshold.

| Gate | Test |
| --- | --- |
| G1 | From 42 px black silhouettes, side and head-on, the owner and 5 or more testers (benchmark quiz) tell campaign classes apart at 80% or better. |
| G2 | A greyscale 90 m hull side-on differs from the horizon sky by at least 15 L* at 2 km in clear and 1 km in haze and night (haze fogs 62% by 2 km). Storm and fog are exempt. |
| G3 | A 5° list reads on the boot-topping band at 300 m, and on a 30 m mast at 1 km. |
| G4 | At 300 m, plating edge density (Sobel) is at most half the weapons'. |
| G5 | No 360° capture of any preset has a 90° sector of uniform horizon. |

Banned in the 3D world, in addition to the UI bans:

| Tell | Check |
| --- | --- |
| Lines on every cell boundary; normals smoothed across cells | Mesher test: shape-table normals only |
| Saturated blocks, uniform saturation, per-face random tint | Hull albedo within token Y and C limits |
| Default ProceduralSky; empty horizon | Lint; G5 |
| Aliased or pure-black shadows; flat ambient | 5° sun slow roll: no stair-steps within 100 m; ambient floor |
| Bloom on non-emissive surfaces, auto exposure, chromatic aberration, grain, lens dirt, vignette, lens flare | Lints |
| DOF in a sortie (miniature look); teal-orange grades, toon ramps, outlines or edge-line passes, cyan glow, combat motion blur | DOF only in editor and replay cameras; review |

### Renderer and post stack

Forward+ runs on every tier. Mobile is rejected: fixed agx_white, no SSAO, TAA or MetalFX temporal, 8 lights per mesh. Compatibility (OpenGL 3) is a logged safe mode with a persistent update-driver notice, keeping every gameplay cue, exempt from art review, with its own goldens.

- Windows: `driver.windows = "d3d12"` set explicitly (the class default is vulkan); both fallbacks kept. 4.7's D3D12 driver has no pipeline cache. S2 makes Vulkan the default if reports show D3D12 over 10% slower or hitching ([#123125](https://github.com/godotengine/godot/issues/123125)).
- Floating origin: everything renders as f32 relative to an f64 render origin, re-centred (snapped to 1 km) when the camera strays 4 km. Old VFX anchors live 30 s past a rebase.
- macOS: metal, `min_macos_version_arm64 = 14.00` ([#123266](https://github.com/godotengine/godot/issues/123266)), the macOS floor.
- Ubershaders and the shader baker on. Before each tag a baked Mac export launches on the owner's Mac with empty `user://shader_cache` and Metal caches; on "Not enough bytes for uniform" ([#124539](https://github.com/godotengine/godot/issues/124539)) or a first frame over 10 s, it ships unbaked behind a precompile screen. Windows bakes on a hosted runner (editor non-headless, WARP); a failed bake or a draw-time compile ships unbaked the same way.

| Setting | Low (base M1) | Medium (Windows minimum) | High |
| --- | --- | --- | --- |
| Glow | Levels 2 to 3, linear | Levels 2 to 4, bicubic | Same |
| SSAO | Off | Low, half size, radius 1 m, fade 50 to 300 m | Medium |
| SSR | Off | Off | Water only: half size, 32 steps |
| Volumetric fog | Off | Off | Searchlights, flares and muzzle light: 96 m |
| AA | MSAA 2x, internal resolution | MSAA 2x (SMAA 1x where auto-detect finds MSAA over 1.0 ms) | MSAA 4x |
| Upscale | MetalFX spatial (FSR1 on Windows) | None (Apple: MetalFX spatial) | Above 1080p, FSR1 or MetalFX spatial at 0.77; temporal after gate T |

Every tier:

- AgX with one 17³ LUT per preset (33³ on High); roughness limiter and debanding on.
- Glow blend mode 1 (Screen, before tonemapping since 4.6), `glow_hdr_threshold` 1.0, bloom 0.
- `tonemap_agx_white` 8.0 and `agx_contrast` 1.25, global. Presets vary exposure, light, fog and LUT; auto exposure off. If the hero golden puts flash, tracer, fire and ambient emission under 10 L* apart, `agx_contrast` steps 0.05 within 1.1 to 1.5 until they separate. Preset changes CPU-blend the two LUTs into one ImageTexture3D at 1 Hz.
- HDR output off in v1, since the LUT does not support it.
- Always off: SSIL, SDFGI (38.6 ms with camera motion on an RX 550), VoxelGI, LightmapGI, planar reflections, ocean refraction, motion blur.
- `background_mode` BG_SKY, never Canvas, so glow and LUT never reach the HUD's CanvasLayers.

No default uses temporal AA or motion vectors; TAA, MetalFX temporal on Low and FSR2 at 1080p need gate T: TIME replica within 1 ms, a forced origin rebase smearing at most one frame, no stale motion on reused VFX slots (in captures), and MetalFX temporal at most 1.5 ms on a 7-core M1 (median of 5 base-M1 reports). Temporal AA runs the ocean vertex shader twice.

### Lighting and time of day

Weather is replicated server state from the seed: sea state, wind, cloud, visibility V, rain. Fog density is 3.912/V (Koschmieder), so the render never out-sees the sim.

| Weather | V | fog_density | Aerial perspective | Sea state | Sky |
| --- | --- | --- | --- | --- | --- |
| Clear | 30 km | 1.3e-4 | 0.7 | 0 to 4 | Scattered masses |
| Haze | 8 km | 4.9e-4 | 0.5 | 2 to 4 | Scattered |
| Overcast | 12 km | 3.3e-4 | 0.5 | 3 to 5 | Full layer |
| Rain | 2 km | 2.0e-3 | 0.2 | 4 to 6 | Full, rain streaks |
| Storm | 1.5 km | 2.6e-3 | 0.2 | 7 to 8 | Front, lightning |
| Fog bank | 400 m | 9.8e-3 | 0.0 | 0 to 3 | None |

Times: dusk (sun 3 to 12°), day (25 to 60°), night (moon). Build order: dusk-clear, dusk-overcast, dusk-storm (the vertical slice), day-clear, night-clear, then the rest; the first public release has six weathers at dusk and day plus night-clear and night-overcast (14), 1.0 the other four nights. Day and night console palettes are cut: the HUD keeps dusk tokens under every preset, checked per preset by HUD rule 18.

- The sun is fixed per sortie at the theatre hour: no radiance re-bakes or shadow crawl. Skirmish and authored missions start at 3 to 25° about 60% of the time.
- Weather blends over 20 to 60 s; presets set sun colour and energy, sky, ambient above a floor, fog colour and exposure.
- `fog_sun_scatter` = 0.2 × (1 − smoothstep(10°, 25°, sun elevation)). `fog_height_density` = 0: Godot's height fog is not ray-integrated and would fog near hulls evenly.
- World Camera3D.far is at least 20,000 m (default 4,000).
- Night: the moon is a shadow-casting DirectionalLight3D, the sky keeps a minimum value, and emission follows emission control as the brightest pixels.
- Lightning: an 80 to 150 ms pulse, at most every 2 s, with an off toggle.

| Shadows | Splits | Distance | 16-bit atlas | Filter | Shadowed lights |
| --- | --- | --- | --- | --- | --- |
| Low | 2 | 250 m | 2048 | Soft Low | 0 |
| Medium | 4 (2 where auto-detect finds over 2.0 ms) | 500 m | 4096 | Soft Low | 4 |
| High | 4 | 800 m | 4096 | Soft High, PCSS option | 16 |

`shadow_blur` stays at 1.0. The caster mask excludes the VFX layer.

### Ocean and sky

Sim-core owns the waves and the shader draws its set: 12 Gerstner components (8 or 16 if sim-core's wave gate picks them; cap 16), λ in 8 to 400 m, sets with Σ Q·k·A > 0.85 rejected. A Pierson-Moskowitz generator sets Hs per WMO code 2 to 8 at 0.3, 0.9, 1.9, 3.2, 5.0, 7.5 and 11 m, plus 2 swells, tested within 5%. Every tier draws every component, bar a Nyquist fade A × smoothstep(2s, 4s, λ) allowed only while faded amplitude under any ship is at most 0.001 × camera distance.

Each frame gdclient computes φ_i = wrap(k_i·d_i·O_xz − ω_i·t_r + φ0_i) in f64 (O the render origin, t_r interpolated sim time); shaders use θ_i = k_i·d_i·p_xz − ω_i·wrap(TIME − t_ref) + φ_i with origin-relative p.

Phases travel as 8 global mat4s plus t_ref (600 B per frame). A Rust build step emits `res://shaders/gen/waves.gdshaderinc` from sim-core's packing, included by water, hull and VFX shaders. `time_rollover_secs` = 2048. The t_ref replica, an ALWAYS node that runs first, adds the already time-scaled `delta` and commits only when `Engine.get_frames_drawn()` advances. No low-processor mode.

Tests: a Rust f32 port of the include matches sim-core's f64 libm within 2 mm (t to 3 h, origin offsets to 50 km, |p| to 12 km); a GPU residual render (nightly on WARP, each tag on metal) stays under 1 cm; Newton `height_at` reaches 1 mm in 4 steps.

The mesh is a Rust geometry clipmap: levels snap to 2s in f64, morph over the outer n/10 band, and a skirt reaches the far plane.

| Tier | Levels | n | s0 | Extent | Triangles |
| --- | --- | --- | --- | --- | --- |
| Low | 8 | 63 | 1.0 m | ±4 km | 48k; sag ≤ 7.7% of A on 8 m waves |
| Medium | 10 | 63 | 0.5 m | ±8 km | 55k |
| High | 9 | 127 | 0.5 m | ±8 km | 222k |

Detail is a Rust-baked Tessendorf FFT looping every 24 s (quantised dispersion): 64 frames of 256² BC5 normals plus BC4 turbulence, 8 MB, sampled at 41 m and 11.3 m, no displacement. No live FFT in v1.

The water material is opaque, with no ALPHA and no screen, depth or normal reads, so it writes depth and takes SSAO and SSR. Its light() follows [Atlas](https://gpuopen.com/gdc-presentations/2019/gdc-2019-agtd6-interactive-water-simulation-in-atlas.pdf): GGX with Walter's Smith term, Fresnel R + (1−R)(1−N·V)^(5e^(−2.69α))/(1+22.7α^1.5) at R = 0.02, and four-term fake scatter. Water normals stay smooth. A 0.08 sun-specular roughness floor turns a low sun into a long glitter path. Cloud shadows are one noise fetch in water and hull shaders.

Foam uses no SubViewport. Whitecaps are the Gerstner Jacobian with 0.6 s and 1.2 s lagged copies times baked turbulence, from sea state 3; calm seas foam only at hulls and wakes. Within 200 m foam edges snap to a 0.5 m world lattice. Contact foam reads the hull mask's waterline SDF; wakes are VFX ribbons; shore foam and shallow tint read a camera-centred R16 heightfield, 2048² at 4 m (8 MB).

Hull masking: up to 8 hulls within 500 m keep an RGBA16F layer (dry-column bottom, top, waterline SDF), 512x128 in hull XZ at 0.5 m, in a 4 MB Texture2DArray with world-to-hull mat4s. Water discards inside the column (keeping the opaque pass). Layers rebuild on cell change, at most every 100 ms. Flooded compartments draw their own water. No stencil (experimental in 4.7).

Hull shaders within 300 m include the wave include and wet cells below h + 0.3 m to roughness 0.2. The orbit camera stays 1 m or more above the surface. No underwater rendering through 1.0.

Sky: a PhysicalSkyMaterial (radiance 256, INCREMENTAL) at milestone 0, then by the vertical slice a custom sky shader without TIME or POSITION, so radiance updates only on preset change: atmosphere, sun disc, stars, moon lit from LIGHT1.

Clouds are faceted opaque meshes: low-subdivision icosphere clusters lit per vertex with a wrap term, drawn in the main pass and fogged, never blurred (blur erases facets). Layout comes from the server seed and drift from a synced wind offset. A banded cirrus layer in the sky shader steps at most every 2 s. No volumetric clouds.

Islands: flat-shaded triangles (8 m near, 32 m far), token colour bands, same fog.

### Ship meshing and materials

Hulls render as meshed 16 m chunks, not MultiMesh cubes (they draw every hidden face); greedy meshing cuts a solid 8³ block from 3,072 quads to 6 ([0fps](https://0fps.net/2012/06/30/meshing-in-a-minecraft-game/)). A moving ship costs one transform per chunk. MultiMesh stays for parts, projectiles and VFX.

Shapes are cube, 45° wedge, corner and inverse corner in 24 orientations, plus placeable 1:2 slopes. The loft voxelizer samples the hull SDF at each boundary cell's 8 corners, and a 256-entry table maps the pattern to the nearest shape by Hamming distance, ties to cube, so curved hulls read as faceted steel, not stairs. The sim may treat shaped cells as full.

The Godot-free `hull-mesh` crate uses binary-greedy-meshing `= 0.6.2` with `Mesher::<32>` (vendored if a 32-chunk equality test fails), then a per-cell pass emits shaped faces from a face table with neighbour-cover masks. Greedy input is a class buffer: 0 exposed air, 1 opaque, 2 glass, 3 emissive, shaped classes, 0xFFFF sealed air (no faces); a breached or entered compartment's air becomes 0. Chunks are 32³ cells padded to 34³, grid-aligned, so a 90 m hull is 12 chunks; budget 200 µs per chunk on one minimum-spec core (S5). Tests cover triangle count, watertightness and byte layout.

Surface bytes: `format = VERTEX | NORMAL | INDEX | ARRAY_FLAG_FORMAT_CURRENT_VERSION = 34359742467`; `vertex_data` is N × 12 B f32 positions then N × 4 B octahedral normals (2 × u16 UNORM); u16 indices up to 65,536 vertices, else u32; clockwise front faces; no UV, tangent, colour or compression at LOD0.

A rebuild makes a new RID with `mesh_create_from_surfaces` plus a welded position-only shadow mesh, swapped in with `instance_set_base`. Old RIDs free 2 frames later, at most 4 per frame, from a queue that may lag. Never `mesh_clear` a visible chunk; it frees at once.

Each chunk has a mipless 34³ RGBA8 Texture3D (157 KB): R occupancy, G swatch (4 bits, the blueprint palette index) and material class (paint, glass, emissive, bare), B scorch, A armour and wear seed, uploaded with its mesh (scorch-only at up to 4 Hz), in a per-chunk ShaderMaterial on one shared shader.

Hull shader: `cull_back`; no VERTEX writes, discard, alpha, `world_vertex_coords` or light() override, so the shadow mesh and Godot lighting apply. Glass is opaque, dark and glossy. 4 to 5 fetches per pixel.

| Term | Rule |
| --- | --- |
| AO | One linear R fetch at face + 0.5 cell × normal. AO = 1 − 0.45 × clamp(1.33 × occupancy, 0, 1). S6 switches to 0fps vertex AO if it fails. |
| Bevel | Within 0.04 m of a convex exposed edge, bend the normal toward the edge and lift value 10 to 15%. Fade it over 2 to 1 px (19 to 37 m) with fwidth. |
| Seams | Strakes 1 m, plates 4 m with butts staggered 2 m, frames every 2 m, exposed flat faces only: 1 px, 8% darker, fwidth-antialiased, fading over 117 to 234 m; half contrast on superstructure. |
| Weathering | Seeded per ship and cell: rust bleed = edge mask × 1:8 vertical noise × level; a salt band 0.6 m above water, 10 to 15% darker; streaks below fittings. At most 20% value, 0.03 chroma; level 0 in the editor. |
| Damage | Removed cells vanish. Exposed interiors show primer (Y 0.25, warm, C ≤ 0.06). Scorch takes value to 40% and chroma to 0. |

LOD: Rust picks per vessel per frame the coarsest LOD whose cell is at most 1.5 px (3.0 on Low), 15% hysteresis; no visibility-range fades (they force transparency). At 60°, LOD1 (1 m) starts at 623 m, LOD2 1,247 m, LOD3 2,494 m, LOD4 4,988 m. LODs downsample any-solid into one mesh each, with swatch and AO in vertex colour. No impostors.

Repeated parts are one MultiMesh per type, in shadowed near and unshadowed far bands. Undamaged same-blueprint ships share chunk meshes. A meshing thread outside the sim's 3-core pool prioritises by screen area (×4 player, ×2 its target, up to 8 remeshes per frame for those); holes show within 2 frames inside 1 km, 10 beyond. T-junction cracks are fixed by splitting quads in the mesher, never in the vertex shader (it breaks the shadow mesh).

### VFX

Layer A runs on every renderer: one MultiMesh per VFX mesh type, one `multimesh_set_buffer` per frame, 80 B per instance. Rust writes age into `custom.x`, never derived from TIME, which rolls over. Opaque types use fixed ring slots; a reused slot draws at scale 0 for a frame. Rust sorts blended types back to front.

Layer B, Forward+ only, is pooled GPUParticles3D for secondary detail. Spray, sparks, embers, flak and mist each get a seed emitter plus a sub-emitter per anchor (20 nodes at most), with at most 128 `emit_particle` calls per frame, `local_coords` true (linted), `fixed_fps` 30 interpolated, INDEX order, no trails, collision nodes or shadows, and turbulence on at most 2 emitters. Anchors follow the render origin; water contact is analytic.

Style: faceted flat-normal meshes of 20 to 80 triangles, continuous motion, a 256x1 colour ramp posterised to 4 to 6 bands, peak in the first 25% of life then ease-out, ending by scaling to 0. Alpha blending only for mist, missile trails and muzzle haze.

Emission ladder: flash 12 to 16 for 2 to 3 frames, tracers and sparks 4 to 8, fire 2 to 4, everything else 1 or less.

Accessibility floor 6: a flash blooming over 2.5% of the screen counts against a global 3 per second; extras in the same third of a second draw at 25% emission with no light. Reduce flashing caps emission at 4 and removes lightning. IRIS checks combat captures.

| Effect | Rule |
| --- | --- |
| Tracers | Opaque emissive boxes from the projectile buffer, speed × 1/40 s long, at least 1.5 px wide. |
| Splash | 3 to 5 white hex prisms: rise 0.6 to 1.0 s, hold 0.4 s, collapse 2 to 3 s, foam ring 6 s. Height 0.25 m per mm of calibre, capped at 40 m. 64 own-side slots never culled. |
| Hit | 2-frame flash, 3 to 7 icospheres ramping white to grey over 1.2 s, up to 64 cells flying as 0.5 m cubes. |
| Fire | Heat glow from volume B, emissive flame prisms, opaque faceted smoke at 2 to 4 puffs/s. |
| Defence smoke | Sim object, same on every tier; opaque puffs. |
| Missile trail | Sorted blended cross-quads every 0.1 s, 12 s long (6 s on Low); the 2 s head is never trimmed. |
| Wake | Ribbon 1.0 to 2.5 beams wide for 30 s; Kelvin arms at 19.47° above 8 kn; bow wave by speed. |
| Muzzle | Emissive star for 2 to 3 frames, smoke puffs, unshadowed light of ≤ 30 m for ≤ 0.15 s. |
| Flooding | Pump arcs, boil rings at holes below the waterline, a foam band growing as freeboard drops. |

- VFX lights: 16 on Medium and High (8 for fires), 8 on Low, 0 on the fallback.
- Live caps, Medium / Low, hard (the oldest unprotected slot recycles): projectiles 2,000 / 2,000; splashes 256 / 128; fireballs 64 / 32; flame prisms 1,024 / 384; smoke puffs 4,096 / 1,536; debris 2,048 / 512; flak 512 / 192; wake segments 2,560 / 1,280; trail segments 2,048 / 1,024; Layer B particles 32,768 / 8,192.
- At most 12 particle process materials and 24 VFX shaders; loading draws each once and calls `request_particles_process(0.1)` on hidden Layer B copies.

### Quality tiers and budgets

| Tier | Hardware | Target | 3D scale |
| --- | --- | --- | --- |
| Low | Base M1 to M3 under 12 GiB, every MacBook Air, Windows below minimum (warned) | 30 fps (`max_fps` 30, V-Sync) | sqrt(1.0 MP / window MP), 0.49 at 2560x1600; floor 0.7 MP |
| Medium | Windows minimum, budgeted on the RTX 3050 6 GB; fan-cooled base Apple chips with 12 GiB or more | 1080p60; Apple 30 fps at 1.4 MP | 1.0 |
| High | RTX 3060 Ti, 4060, RX 6700 and up; M Pro, Max and Ultra | 60 fps at 1080p, or 1440p at 0.77 | 1.0 or 0.77 |
| Fallback | Compatibility renderer | Best effort | Layer A only, MSAA 2x |

Scale comes from the physical viewport, clamped to MetalFX limits (`limit_get()/1e6`).

| GPU pass (budget, ms) | Medium: 3050 6 GB class, 16.7 ms | Low: 7-core M1, cold, 33.3 ms |
| --- | --- | --- |
| Prepass and opaque | 3.0 | 6.0 |
| Shadows | 2.0 | 3.5 |
| Ocean | 2.0 | 5.0 |
| Sky and clouds | 0.5 | 1.0 |
| VFX | 2.0 | 3.5 |
| Post | 4.0 (MSAA 1.0, glow 1.7, SSAO 1.1, rest 0.2) | 2.5 |
| HUD | 0.5 | 1.0 |
| Slack | 0.5 | 1.5 |
| Total | 14.5, leaving 2.2 ms | 24.0, fits after a 0.72 thermal derate |

High: 14.0 ms at target resolution; SSR at most 1.0 ms, volumetric fog 0.4, MSAA 4x 1.2, SSAO Medium 1.2.

Performance budgets' 2 ms main-thread adapter budget splits:

| Work | ms |
| --- | --- |
| Snapshot and interpolation | 0.6 |
| Transforms | 0.15 |
| Part MultiMesh uploads | 0.3 |
| Projectile and Layer A uploads (about 14,600 instances, 1.17 MB) | 0.4 |
| Mesh and volume uploads and frees (hard timer, overflow defers) | 0.3 |
| Wave uniforms, LUT and emits | 0.1 |
| Slack | 0.15 |

Gate: p99 ≤ 2.0 ms in the reference battle (above spike 1's 10,000 instances). Fallbacks in order: GPU-side interpolation from 30 Hz uploads; per-chunk dirty MultiMeshes; culling debris and decor beyond 3 km, never tracked objects.

VRAM (`VIDEO_MEM_USED`): 1.2 GB Low, 3.5 GB Medium, 5 GB High; ocean and sky under 64 MB. Both fit Performance budgets' 4.5 GB Windows VRAM and 4 GB combined M1 caps.

Auto-detect, at first launch and on any adapter or driver change:

1. Read adapter, driver, processor, `OS.get_model_name()` and physical memory; on Windows, DXGI DedicatedVideoMemory via Rust.
2. Macs map by chip, memory and a fanless table (MacBookAir10,1, Mac14,2, Mac14,15, Mac15,12, Mac15,13, Mac16,12, Mac16,13, later Airs). Windows maps by adapter regex; under 6 GiB VRAM or integrated means Low.
3. Run a 10 s scripted flotilla (warming pipelines), then 2 s A/Bs of MSAA 2x against SMAA 1x and 4 against 2 splits (GPU time; Macs, frame time without V-Sync). Over budget at p95 drops one tier, once. Save to `user://settings.cfg`; the player can override any of it.

Timing: D3D12 and Vulkan use `viewport_get_measured_render_time_gpu`; Godot's Metal GPU timer is unusable in 4.7, so Macs use p95 frame time and missed vblanks. Dynamic resolution multiplies the tier's base scale by 1.0, 0.87, 0.77 or 0.67 down to a floor (Low 0.7 MP, Medium 0.77, High 0.67), with FSR1 on Windows and MetalFX spatial on Macs below 1.0: down after 1 s over 90% of budget, up after 5 s under 70%, 3 s apart. Steps that hitch over a frame wait for camera cuts. Macs poll `thermalState` at 1 Hz: serious drops a step and 30% of particles; critical drops to the floor with glow off (warmed at load). MSAA, shadow precision, atlas size and SSAO change only in settings.

Degrade order, rung 1 (visual detail) of Performance budgets' ladder; later rungs follow step 6:

1. Resolution down to the tier floor.
2. Debris halved, then 16 per event; Layer B sparks, embers and spray off; smoke halved beyond 3 km.
3. Layer B at 15 fps; trails trimmed to the head; Kelvin arms off; splashes beyond 8 km as one prism.
4. VFX lights for own guns only; shadow distance ×0.75; non-player hulls beyond 300 m cast no shadow.
5. Glow to level 2, then off.
6. Next tier's clipmap; one detail tiling.

Never degraded: own fall-of-shot splashes, incoming missile heads, live tracers, hit flashes, fire presence, flooding and list cues, defence smoke, the wave set and waterline match.

If a tier misses budget in development or a class fails the benchmark rule, cut in this order, one per patch: linear glow upscale, SSAO off, 2 shadow splits, clipmap n, VFX caps.

### Verification

No test hardware is owned. Hosted runners check correctness, not milliseconds; Windows renders via D3D12 WARP and, where available, Mesa lavapipe (vulkan) and llvmpipe (opengl3). Every PR: boot per driver, lints, `RENDERING_INFO_PIPELINE_COMPILATIONS_DRAW` flat after load and at each VFX type's first emission (the Mac run also fails a first-emission frame over 50 ms). Nightly: the reference-battle replay fails a tier over its VRAM cap or 10% over baseline primitives or draw calls. macOS arm64 boots a Metal export where the runner has Metal.

Goldens and the wave residual: scripted replays with `--write-movie --fixed-fps 60`, nightly on hosted Windows, weekly and per tag on the owner's Mac (metal), covering Platform support's five scenes plus each shipped preset from 15 m, 60 m, 300 m and 2 km, per driver. FLIP fails a mean above 0.05; a capture failing 2 of its last 8 runs with no bug moves to 0.08 once, then to a static frame. The owner approves new goldens. Sets add greyscale and deuteranopia copies (G2, G3, HUD rule 18) and the A0 capture.

Throttle profile: the owner's Pro/Max Mac stands in for the 7-core base M1, rendering Low at 1.0 MP × k internal pixels (3D scale up to 2.0), k = GPU cores / 7, V-Sync off, timed with Instruments or `MTL_HUD_ENABLED=1`, never Godot's timer; Godot's worker pool caps at 4 threads beside the sim's 3 (the M1's 4 performance cores). Vertex work and faster cores ignore k, so Low passes at frame p95 ≤ 20.4 ms (85% of 24.0) and adapter p99 ≤ 1.0 ms, single-core work at half budget; after 3 base-M1 reports, k becomes their median p95 over the Mac's unthrottled p95 and the frame gate 24.0 ms. Weekly and per tag it replays the reference battle through smoke and a close splash; per-pass A/B deltas at p95 over 60 s fail any pass 20% over its Low row.

Benchmark scene (main menu): a fixed-seed 60 s reference-battle replay at the detected tier, Low and Medium, then a relaunch under vulkan on Windows or a MetalFX temporal A/B on Macs, an optional 20-minute fanless-Mac soak, and the G1 quiz. Its report (adapter, driver, OS, model, tier, frame and GPU p50/p95/p99 per run, adapter p99, `VIDEO_MEM_USED`, peak memory, pipeline compiles, thermal trace) goes in a GitHub issue form. A class (RTX 3050 6 GB or 2060 for Medium, base M1 8 GB for Low) passes when its 3 latest reports' median meets budget (Medium GPU p95 ≤ 14.5 ms, Low frame p95 ≤ 33.3 ms also after the soak, adapter p99 ≤ 2.0 ms); a failing class gets the cut order until it passes. Below 3 reports, defaults ship; per-machine auto-detect and dynamic resolution still apply.

Owner art review each milestone: G1 to G5, the ban table, the 5° slow roll, and ocean and bevel shimmer at 15, 60 and 800 m.

Spikes gate content in Milestone 0's time box; an unanswered one ships its default.

| Spike | Default | Gate | Fallback |
| --- | --- | --- | --- |
| S1 | Ocean as specified | 40 hulls, sea state 5, dusk, rain toggled, 15, 60, 800 m cameras; Low row, throttled | One detail tiling, then next-lower clipmap |
| S2 | D3D12; MSAA 2x | Median of 10+ Windows reports (both drivers): D3D12 over 10% slower or p99 hitch over 50 ms | Vulkan default next patch; AA A/B per machine |
| S3 | Temporal off | Gate T | Stays off |
| S4 | Adapter split | gdext costs of transforms, `mesh_create_from_surfaces` at 100 to 1,000 KB, `texture_3d_update`, free_rid; throttled | Adapter fallbacks |
| S5 | Mesher::<32> plus shaped pass | 3 hulls, 200 µs per chunk (100 µs on the Mac) | 4 remeshes per frame; holes within 4 frames inside 1 km |
| S6 | Volume AO | Within 0.5 ms of vertex AO, throttled | 0fps vertex AO |
| S7 | Listed caps | GPUParticles3D at 1k to 32k within the VFX row; baker covers particle shaders | Caps halve; warm-up draws unbaked shaders |

Lints fail CI on: `glow_hdr_threshold` below 1.0; `glow_blend_mode` not 1; `auto_exposure_enabled = true`; `fog_height_density` not 0; aerial perspective above 0.3 at density 5e-4 or more; world camera far below 20,000; `local_coords = false`; depth or screen hints in water or VFX shaders; world colour literals outside tokens; `mesh_clear` in gdclient; the strings chromatic, grain, lens_dirt and vignette; a missing or changed project.godot renderer key.

## Sharing and mods

At the first public release (GitHub, version 0.x), players share blueprints and kits as blueprint files and copyable share strings, and nothing else is moddable. Steam Workshop is a phase 2 feature that exists only if the game goes to Steam. The data formats are documented on the project's GitHub Pages documentation site from the first public release, so that data-only part packs can follow after 1.0.

### Blueprint format

A blueprint is a versioned, content-hashed file with a RON text form that diffs cleanly outside the game. Ships save as `.bp.ron` and kits as `.kit.ron`, UTF-8, under `user://blueprints/`. The project turns on Godot's custom user directory named Black Pennant, so the folder is `~/Library/Application Support/Black Pennant/blueprints` on macOS and `%APPDATA%\Black Pennant\blueprints` on Windows. Blueprint folders in the editor are subfolders there, and imported items land in `Imported/`.

A file holds:

- a header: `format` (an integer, starting at 1), the game version that wrote it, `hash`, name, author display name (optional), `licence` (an SPDX id, default `CC-BY-4.0`, editable in the export dialog and shown to anyone who opens the file; the game records it and does not enforce it) and `lineage` (the 32 most recent ancestors, each a hash plus author name);
- the hull: lofted shell parameters, or freeform cells after a bake, plus decks and bulkheads;
- cells, stored per 0.5 m row as run-length runs of material, armour tier, paint index, and the shape and rotation byte that boundary cells carry (wedge, corner or inverse corner, from The editor), so a hull edit changes only the rows it touches in a diff;
- a paint table of at most 16 entries, because the renderer stores a 4-bit swatch index per cell. New blueprints start with the 8 USN swatches and the antifouling colour from Graphics and rendering. Custom entries come from the editor's picker, clamped to Y 0.6 or below and OKLCH chroma 0.06 or below for hull paint, and Y 0.7 or below for markings. The loader re-applies the clamps, so a hand-edited file cannot exceed them;
- parts and kits with their socket bindings, then tuning, rules tables, graphs and Luau script sources.

The hash is BLAKE3 over the canonical RON that the game's serializer writes (fixed field order, LF line endings, no comments), with the `hash` field empty. The same hash addresses blueprints on the network (Multiplayer > Blueprints). A file edited by hand is re-canonicalized on load and saved with its new hash; a mismatched hash in a file is recomputed, not an error. A share string whose decoded content does not match its hash is refused as damaged.

Every file and share string is untrusted input. The loader refuses, before parsing, anything over 16 MiB of RON, any zstd frame that would expand past 16 MiB, and RON nesting deeper than 128. Then it runs the server's validation (schema, part legality, ruleset limits, bounds and connectivity) and recomputes every stat itself.

Every format change ships with an explicit migration, and CI loads a corpus of saved ships through every migration on Windows and macOS. Each format bump adds one migration from version n to n+1, and migrations are never deleted, so a version 1 file loads in every later release. A part removed from the game migrates to its named replacement; if it has none, its cells become empty and Ship's Notes lists each removed part. A file whose format is newer than the running game is refused with a message naming the release it needs; the loader never drops unknown fields silently.

The migration corpus is every stock blueprint and kit, the trial-battery ships, a set saved by each tagged release, and any blueprint a player attaches to a GitHub issue labelled `blueprint-corpus` and agrees to add under CC BY 4.0. Gate, on every pull request on the Windows, macOS and Linux hosted runners: every corpus file loads, migrates and validates, and its recomputed stats (tonnage, cost and the editor gauges) are diffed against the previous release. A file that fails to load or validate fails the job. A stat change fails the job unless the pull request lists it in the release's Blueprint changes notes, which the release notes print in full. In Stormworks' top-rated negative reviews, about half the writers say an update broke their builds, Workshop vehicles or saves, so a ship that worked before an update keeps working after it, or the patch notes say exactly why not.

Kits keep version and author lineage. An expert who forks the Gunnery officer and publishes the kit shows up to other players as an L0 or L1 preset: a name, a description and a few sliders. Forked officers and graph edits are allowed under the open PvP ruleset and banned under standard, the default, so such a kit is fielded in PvP only in open lobbies.

### Sharing before Steam

Export writes a `.bp.ron` or `.kit.ron` file. Import takes a file from the Import button, from a drop onto the game window, or from a file placed in the blueprint folder (the browser rescans when the window regains focus).

A share string is the prefix `BPN1:` followed by base64url without padding (RFC 4648, section 5) of a zstd frame (level 19) of the canonical RON. The digit is the share-string version, separate from the blueprint `format`; the content says whether it is a ship or a kit. Copy is a button in the editor and the blueprint browser. Pasting a string anywhere in the editor or the browser (Cmd+V on macOS, Ctrl+V on Windows) opens an import preview, which renders the ship in-engine from the data before the player confirms. Whitespace and line breaks inside a string are ignored, because chat clients wrap long text. The copy dialog shows the string's length and, above 4,000 characters, also offers Save as file.

Players post strings and files wherever they choose, and the repository's GitHub Discussions has a Blueprints category. Files and strings carry no images; every preview is the game's own render of the blueprint data.

The project runs no service that stores player content, so it is not a hosting provider for it under the EU Digital Services Act ([EU Digital Services Act](https://digital-strategy.ec.europa.eu/en/policies/digital-services-act)). GitHub stores Discussions posts and carries the notice-and-action duties for them under its own terms; the owner moderates the Blueprints category with GitHub's tools. Ship names, lobby names and chat seen in a match are relayed by the player who hosts, over direct UDP (renet2) or, with the Steam feature, Steam Datagram Relay. The project stores none of it. Players can mute and hide other players' names and chat locally. This is a reading of the regulation, not legal advice.

### Workshop

Workshop is phase 2. It is built behind the `steam` cargo feature (off by default) and ships only in Steam builds; files and share strings keep working beside it. Steam Workshop hosts the files, uploads go through ISteamUGC, and the game validates every download against the same rules the server uses. Thumbnails render from the blueprint data on the client instead of being uploaded images, which removes arbitrary images, the largest moderation risk in user content. The game renders the Workshop preview image itself at upload, offers no image picker, and in game always shows its own render of the downloaded data, never the stored preview.

Workshop items (blueprints and kits, with their titles, descriptions and files) are stored and shown by Valve. Valve is therefore the hosting provider for them under the DSA ([Regulation (EU) 2022/2065](https://eur-lex.europa.eu/legal-content/EN/TXT/HTML/?uri=CELEX:32022R2065), Art. 3(g)(iii)) and carries the notice-and-action duties of Articles 16 and 17. Steam already meets them in form: a flag icon on every page and Steam Support tickets, both open to logged-in and anonymous users and routed to Valve's moderation team ([Valve 2025 TCO transparency report](https://cdn.steamstatic.com/apps/valve/reports/2025_tco_transparency_report.pdf)). Valve has an EU legal representative in Germany, and the Bundesnetzagentur opened DSA proceedings over Steam's notice handling in June 2026 ([Bundesnetzagentur](https://www.bundesnetzagentur.de/SharedDocs/Pressemitteilungen/EN/2026/20260612_DSC.html)).

The game adds no DSA reporting flow of its own. It does three things:

1. Every screen that shows another player's Workshop item has a Report action. It opens that item's Workshop page in the Steam overlay, where the flag is.
2. The owner moderates the game's Workshop hub with Steam's tools (hide as incompatible, hide as inappropriate, ban), and the game refuses any item hidden as incompatible or banned ([Steamworks community moderation](https://partner.steamgames.com/doc/marketing/community_moderation?l=english)).
3. The game shows the Steam Workshop Legal Agreement next to the submit button, because items stay hidden until the author accepts it ([Steam Workshop implementation](https://partner.steamgames.com/doc/features/workshop/implementation)).

Trigger. Any project-run service that stores player content would make the project a hosting provider. Examples are project-hosted ranked servers, a leaderboard that shows ship names, or a blueprint site run by the project. None is planned, and with zero cash spend none runs. Adding one is a scope change that needs the owner's approval, and it ships only with: an electronic notice form with the Art. 16(2) elements; statements of reasons under Art. 17; points of contact under Art. 11 and 12; an EU legal representative under Art. 13 if the project has no EU establishment; and terms that describe moderation, under Art. 14. No size exemption covers these: micro and small enterprises are exempt only from the yearly transparency report (Art. 15(2)) and from the online-platform section (Art. 19).

### Executable content

Kits may carry graphs, which run in the game's own bytecode VM and cannot reach the host machine. Luau scripts are different: a host runs every guest's scripts, so a sandbox escape in mlua or Luau would run on the host's computer. The lobby script policy:

| Context | Luau scripts |
| --- | --- |
| Single-player | The player's own scripts run |
| Co-op, skirmish and sandbox lobbies | Off by default. The host can turn them on for lobbies they host. Guest scripts run only after the fuzzing gate below passes; until then the host runs only its own player's scripts |
| PvP, open and standard rulesets | Never |

Scripts inside an imported file, share string or Workshop item arrive disabled. The kit shows its script source, and the player enables that kit's scripts by hand. Graphs need no such step.

The project pins mlua and its vendored Luau (versions in Scripting) and fuzzes the host API surface in CI with cargo-fuzz (0.13.x), libfuzzer-sys (0.4.x) and the arbitrary crate ([cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz)). libFuzzer needs a Unix-like OS and a nightly compiler, so fuzz jobs run on the Linux x64 hosted runner, with a dated nightly pinned in `fuzz/rust-toolchain.toml`, separate from the project's stable pin.

Targets, in `fuzz/fuzz_targets/`:

1. `host_api`. An Arbitrary-derived sequence of host calls is rendered to a Luau script. Function ids come from the host API registry. Arguments are typed and include edge values: NaN, plus and minus infinity, huge and negative counts, deep and cyclic tables, long strings and wrong types. The script runs in a fresh vehicle VM set up exactly as in production (Scripting): no os library, the memory limit, the interrupt budget, then `sandbox(true)`, with JIT off.
2. `script_source`. Raw bytes compile as Luau source and run in the same VM, guided by the host API dictionary.

Oracles, checked on every input: no Rust panic, abort or sanitizer report; memory never exceeds the vehicle limit plus a fixed slack; the interrupt budget stops every script within budget; a second run of the same input stops on the same tick with the same output stream; no host function exceeds its declared constant-time or chunk bound, counted in host work units, not wall time; os.clock, os.time and os.date stay unreachable; no value or global leaks between two vehicles' VMs.

Instrumentation. cargo-fuzz instruments the Rust side. The vendored Luau C++ (luau0-src, built by the cc crate) is instrumented by setting CC=clang, CXX=clang++, and CFLAGS and CXXFLAGS to `-fsanitize=address,fuzzer-no-link` in the fuzz job. Clang's major version must match the LLVM version that `rustc -vV` reports. A CI step fails the job if the Luau objects lack `__asan` or `__sanitizer_cov` symbols; if the cc build ignores the flags, that step fails, and the fallback is a build script that compiles luau0-src's sources with those flags directly.

Corpus. Seeds are Luau's tests/conformance scripts (MIT licence, [Luau](https://github.com/luau-lang/luau)), every example and starter-kit script in the repo, and minimized crash repros. `fuzz/host_api.dict`, generated from the host API registry, lists every host function name and Luau keyword. The corpus persists through actions/cache, keyed by target and the pinned mlua version, and `cargo fuzz cmin` minimizes it weekly. Luau's upstream fuzz harness (libprotobuf-mutator over luau.proto) targets the parser, compiler and VM rather than this host API, so the project does not run it.

Schedule, all on free standard hosted runners:

- Every pull request that touches the Luau host or the mlua pin runs each target for 120 s.
- Nightly, each target runs in its own job for 5.5 hours (`-fork=4 -max_total_time=19800` on the 4-vCPU Linux runner, inside the 6-hour job limit). That is 22 CPU-hours per target and 44 per night, about 1,320 in 30 days.
- A crash fails the job, uploads the input, and becomes a regression test in `cargo test`, which also runs on Windows and macOS.

Gate for guest scripts (the L4 criterion in Scripting): at least 30 consecutive days and 1,000 CPU-hours of fuzzing on the pinned mlua and Luau with zero sandbox escapes (any file, network, os or debug access, or any write outside the VM), zero host crashes or panics, zero ticks stopped only by the wall-clock backstop, at least 90% line coverage of the host API crate (cargo-llvm-cov), and every published Luau and mlua security advisory reproduced as a passing regression test. Guest scripts unlock in the first release after the gate passes. A later mlua or Luau upgrade restarts the 30-day count but does not re-lock guest scripts, because the upgrade's own nightly runs must stay clean; any escape found takes the 72-hour path below.

Security window. A hotfix ships within 72 hours of an upstream fix for a sandbox escape. If no upstream fix exists yet, a hotfix that disables guest scripts ships within 72 hours of the project learning of the escape. Other mlua and Luau security fixes ship within 14 days. Each hotfix is a GitHub release with a GitHub Security Advisory; with the Steam feature, the same build goes to the default branch.

Builds from GitHub do not update themselves, so a hotfix alone leaves old hosts exposed. When a host turns on guest scripts, the game fetches a static `security/scripts.json` from the project's GitHub Pages site, listing the releases that must not run guest scripts. If the running release is listed, or the fetch does not complete within 5 s, guest scripts stay off and the host is told why; the host's own scripts still run. The request carries only the release version.

Forks of the open-source code are allowed under its licence. The official game loads no code, shaders or Godot resource packs from players, and an authoritative host re-validates everything a client sends.

### Part packs after 1.0

The first public release and 1.0 load no part packs. After 1.0 the game loads data-only part packs: RON part definitions that compose the existing procedural mesh generators and stay inside the documented parameter ranges, with no code, shaders, images, audio or Godot resources. A pack goes through the same validation and migration rules as a blueprint. Packs load in single-player and in co-op, skirmish and sandbox lobbies where every player has the same pack set, matched by hash. The standard PvP ruleset allows none; the open ruleset allows packs every player has.

### Licence and price

Black Pennant is open source and free forever.

- Code: MIT OR Apache-2.0. Data (part tables, campaign data, stock blueprints and kits, and any committed rendered art or audio files): CC BY 4.0. Fonts: OFL. Player blueprints belong to their authors under the licence in their header.
- Steam integration lives behind the `steam` cargo feature, off by default. The Steamworks SDK is never committed to the repository; the steamworks crate's build fetches it when the feature is on.
- Every build on GitHub Releases is free. If the owner judges the game really good and takes it to Steam, it is free there too, and the one-time $100 Steam Direct fee is the project's only cash spend.
- All weapons, sensors and combat systems are in the base game. Campaign acts 1 to 4 arrive as free updates during the 0.x releases, and any theatres after 1.0 are free updates too.
- There are no paid expansions, no DLC, no in-game purchases, no ads and no paid tiers.

## Roadmap

Claude builds the game full-time in the public repository, and the owner directs, reviews and playtests. Milestone 0 is a hard 3-month box of throwaway prototypes that answer the hardest technical questions before any content exists, with one extension of up to 1 month. Each later milestone ends on a measured gate and the owner's sign-off, not a feature count.

_Roadmap diagram: in the original Claude design doc, https://claude.ai/artifact/3cQK3H8ckXzt48Dbsi8pbK_

The roadmap has five phases, each closed by one gate: Milestone 0, the vertical slice, the first public release, Acts 2 to 4, and 1.0.

| Phase | Version | Estimate |
| --- | --- | --- |
| Milestone 0: spikes | No release; one benchmark pre-release (0.0.1) | 3 months, plus one extension of up to 1 month |
| Milestone 1: vertical slice | Playtest pre-releases 0.0.2 onward on GitHub Releases | 4 to 6 months |
| First public release | 0.1.0, the first GitHub release not marked pre-release | 4 to 6 months after the vertical slice |
| Acts 2 to 4 | 0.2.0, 0.3.0, 0.4.0 | 3 to 4 months each |
| 1.0 | 1.0.0 | 2 to 3 months after Act 4 |

From the start of Milestone 0 to 1.0, that adds up to 22 to 31 months, or up to 32 with the Milestone 0 extension. The durations are estimates for an AI coding agent working continuously. Code throughput is not the binding constraint. Three things are: the owner's review and playtest time, collecting playtest samples for the gates, and tuning feel, balance and content, where each change has to be played to be judged. Comparable games overran their early access estimates: Sprocket is at about 2.6 times its estimate after a solo 15-month start, while Trailmakers took about 20 months from early access to release with a studio team ([Trailmakers](https://en.wikipedia.org/wiki/Trailmakers)). No dates are published anywhere, so overruns change only this doc.

Replanning is mechanical:

- Every issue gets an estimate in agent working hours when it is planned, and records its actual hours from the session log when it closes.
- Every milestone records two times: the owner's review turnaround, from a review build to the owner's verdict, and the calendar time each playtest gate took to collect its samples.
- At each gate, the build ratio is actual hours over estimated hours for the milestone's issues. The wait ratio is measured review and sample time over the time the estimate assumed.
- If either ratio is outside 0.85 to 1.15, the remaining milestone estimates are multiplied by it, clamped between 0.5 and 3.0. The table above and the roadmap figure are then regenerated.

### Review cadence

CI publishes a review build of main every week, and Claude publishes a milestone candidate when every automated gate in that milestone passes. The owner plays each milestone candidate and signs off, or returns a list of failures that become issues. While a gate waits on the owner, Claude keeps working only on things the gate's outcome cannot invalidate: tests, tooling, bug fixes and the next milestone's engine code. Content that depends on the result waits.

### Where gates are measured

The project owns no Windows PC and buys no hardware. Every gate in this section runs on one of these:

- **Headless CPU gates** (tick time, state hashes, bandwidth, bot matches, campaign bots) run on GitHub's standard hosted runners. For public repositories these are free: Windows x64 with 4 vCPUs and 16 GB, Linux x64 with 4 vCPUs and 16 GB, and macOS arm64 with 3 M1 cores and 7 GB ([GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). The 3-core M1 runner is the base M1 CPU proxy. The Windows runner fits the 3-worker simulation pool plus the main thread. Hosted runners share hardware, so each timing gate takes the median of 3 runs' p99.
- **Rendered performance gates** run on the owner's Apple Silicon Pro or Max Mac under the min-spec throttle profile defined in Platform support. They are timed with Instruments or the Metal HUD, because Godot's Metal GPU timer is unusable in 4.7.
- **Windows rendering correctness** runs on the hosted Windows runner: Direct3D 12 through WARP, and Vulkan and OpenGL 3 through Mesa lavapipe and llvmpipe where available. It checks for errors and compares golden captures. It never gates performance.
- **Real minimum-spec numbers** come from community reports. Every build from the Milestone 0 benchmark pre-release onward contains the in-game benchmark scene. The scene replays the reference battle for 60 s at each quality tier. On Windows it repeats the run under d3d12 and vulkan, and with MSAA 2x and SMAA. It then writes a report: hardware, OS, driver, rendering driver and fallbacks, p50, p95 and p99 frame time, adapter time, tick time, and memory. The tester copies the report as a text string and posts it to the repository's benchmark discussion.
  - A gate counts as confirmed with at least 5 reports from the Windows minimum-spec class (6-core CPU, 16 GB, RTX 2060 or RTX 3050 6 GB class GPU), or at least 3 reports from base M1 Macs with 8 GB.
  - Until a gate is confirmed, it is judged on the throttle profile and the hosted runners, and its default stands. When reports arrive and their median misses the gate, that gate's fallback ladder applies automatically.
  - If the profile's p95 frame time differs from the median of the base-M1 reports by more than 15%, the profile's scale factors are recalibrated to that median and every Mac gate re-runs.
- **Human samples** come from five sources:
  - the owner's own playtests;
  - genre newcomers whom the owner recruits and observes in person or over screen share;
  - pre-release testers' playtest reports, which a local log in the playtest build writes as a copyable string at session end (nothing uploads automatically);
  - Sentry release health, which counts crash-free sessions among players who consented to crash reports;
  - headless campaign bots on the hosted runners.

Sample rule: if a human-sample gate has not reached its required count 6 weeks after its milestone candidate is published, it is judged on the samples collected, provided there are at least half the required count. With fewer, it is judged on the owner's playtests plus the campaign bots, and it re-runs on the next milestone's builds once the count is reached. A short sample alone never holds a milestone.

### Milestone 0 spikes

All six spikes and the graphics spikes run in parallel inside the 3-month box. The pipeline spike may carry into Milestone 1 without blocking it.

1. **Boundary.** A headless Rust simulation on a fixed tick, with Godot as a pure view, draws the reference battle's hull chunk meshes plus 10,000 or more MultiMesh instances (parts, projectiles, VFX) at 60 fps on Metal and Direct3D 12. It measures `multimesh_set_buffer`, `mesh_create_from_surfaces`, `texture_3d_update` and `free_rid` costs from gdext (graphics spike S4), for which no published benchmark exists.
   - Gate: adapter time per frame (snapshot read, interpolation, transforms, MultiMesh, mesh and volume uploads) at p99 of 2.0 ms or less with 10,000 instances. It is measured with tracing spans on the owner's Mac under the throttle profile on Metal, and confirmed for Direct3D 12 by benchmark reports. On the hosted Windows runner with WARP, the same scene must draw with zero errors and match its golden capture.
   - Fallback, in order until the gate passes:
     1. GPU-side interpolation: transforms upload only when a 30 Hz snapshot arrives, and the vertex shader interpolates.
     2. Per-chunk MultiMeshes with dirty flags, so resting instances are not re-uploaded.
     3. An instance cap that culls debris and decorative instances beyond 3 km first, never track-bearing objects.
2. **Ships.** The reference battle and the co-op reference battle run headless with cell hulls, Gerstner buoyancy, flooding, splitting and 2,000 projectiles inside the 8 ms tick budget.
   - Tick gate: total p99 of 8.0 ms or less on both the hosted Windows runner and the hosted macOS arm64 runner, with per-subsystem tracing spans checked against the tick split in Performance budgets.
   - Precision: rapier3d-f64 stays if its overhead over an otherwise identical f32 build is 15% or less, or if the f64 build meets 8 ms p99 on both runners. Otherwise the simulation adopts separate per-cluster f32 worlds.
   - Colliders: the default is cuboid compounds. Rapier's Voxels collider is adopted only if it is at least 20% faster in combined collider-rebuild and contact time over a 10-minute damage soak, gives identical same-platform state hashes across 10 reruns, and keeps the trial battery within its tolerances.
   - State hashes: the spike records the first per-tick Mac and Windows state-hash comparison, which then runs nightly under the promotion rule in Determinism.
   - Fallbacks: the tick ladder in Performance budgets, then the co-op reference battle's base operation shrinks in steps of 5 hulls, as Co-op sets out.
3. **Netcode and AI.** Two player ships and 30 AI units with turning-circle steering fight over the UDP transport (renet2) at 150 ms latency and 5% Gilbert-Elliott burst loss. The spike measures bandwidth per client, including projectile spawn and impact events. The Steam Networking Sockets and relay backend stays designed behind the transport trait and is not built in Milestone 0.
   - Gate: per-vessel replication averages 20 bytes or less per update. Projectile events are 25% or less of client bandwidth. Total per-client p95 is 256 kbps or less, in the spike match and in a replay-driven reference battle. Playable means no correction on the client's own hull larger than 0.5 m, and a manual-aim hit rate of at least 90% of the 0 ms baseline.
   - Fallback: vessels other than the client's own drop to 20 Hz, then projectile spawns batch into one event per salvo. The adaptive 128 to 768 kbps cap applies at runtime.
   - The spike also proves the join path. The owner hosts on a home router, the host maps its port with UPnP IGD, NAT-PMP or PCP, and a pre-release tester on another network joins by join code. If no mapping protocol works on that router, the spike verifies the README's manual port-forwarding and Tailscale paths instead. Every host logs which mapping succeeded.
4. **Pipeline.** CI on the free hosted runners produces an ad-hoc-signed macOS arm64 build (`codesign -s -`) and an unsigned Windows x86_64 build every week, with the Rust library inside. Each build is uploaded to GitHub Releases as a pre-release, with the pull-request matrix gating merges. The same CI also builds the custom Godot export templates with separate debug symbols, and uploads the Rust and engine symbols to Sentry. A weekly job compiles and links the off-by-default `steam` cargo feature and publishes nothing from it.
   - Gate:
     - `codesign --verify --strict` passes on every nested binary.
     - The exported game passes the headless smoke test on all three hosted operating systems.
     - The Windows export runs Godot's Direct3D 12 shader baker from a non-headless editor on WARP.
     - The Metal baker passes the clean-cache launch test.
     - On a clean macOS user account, the owner opens the quarantined zip from GitHub Releases through System Settings > Privacy & Security > Open Anyway and reaches the main menu with the GDExtension loaded.
   - Fallback: if the baker cannot run on WARP, or Godot issue 124539 reproduces on Metal, that platform ships unbaked. Ubershaders, the load-time warm-up scene and the pipeline-compilation frame-time gate then cover it.
5. **Low end.** The spike-2 battle with rendering runs under the throttle profile on the owner's Mac, and as the benchmark scene in the 0.0.1 benchmark pre-release, published in month 2 so community reports can arrive before the box closes. It includes graphics spikes S1 to S7 and the temporal gate T in Graphics and rendering.
   - Gates:
     - The minimum swarm meets the 8 ms p99 tick on both hosted runners using rung 3 of the degradation ladder or below, never rung 4.
     - Host frame rates hold 60 fps on the Windows minimum spec and 30 fps on a base M1. These are judged on the throttle profile until benchmark reports confirm them.
     - The reference battle stays within the memory budgets: a 6 GB working set and 4.5 GB of VRAM on Windows, and a 4 GB `phys_footprint` on a base M1 with 8 GB. The Mac footprint is measured on the owner's Mac. The Windows figures come from reports.
   - Fallbacks: the ordered ladders in Performance budgets.
6. **Editor usability.** Five to ten genre newcomers, recruited by the owner and observed in person or over screen share, use a paper-thin editor prototype (hull handles, kits, Float gauge, splash). The prototype logs every step's time. The spike records a baseline for time to first fight and time to first splash kill, with no pass mark. The vertical slice then has to meet the newcomer gates in The first ten minutes, including first splash kill under 15 minutes.

Graphics spikes, all in the 3-month box:

| Spike | Measured on | Gate | Fallback |
| --- | --- | --- | --- |
| S1 ocean bench (40 hulls, sea state 5, dusk, rain toggled, 15, 60 and 800 m cameras) | Throttle profile; benchmark reports | Each feature within its GPU pass row in Graphics and rendering | The tier's cut order: linear glow upscale, SSAO off, 2 shadow splits, next clipmap, VFX caps |
| S2 Direct3D 12 against Vulkan, MSAA 2x against SMAA | Benchmark reports from RTX 2060 and RTX 3050 6 GB class machines; WARP and lavapipe for correctness | Vulkan becomes the Windows default if the report median shows D3D12 more than 10% slower at p95, or any draw-time pipeline compile after load. SMAA 1x replaces MSAA 2x on Medium if MSAA exceeds 1.0 ms | Until 5 reports exist, D3D12 and MSAA 2x stay the defaults and the rule applies automatically once they do |
| S3 gate T | Throttle profile; base M1 reports with the 7-core GPU | TIME replica within 1 ms, a forced origin rebase smearing at most one frame, no stale motion on reused VFX slots, and MetalFX temporal at most 1.5 ms | No temporal option ships; no default depends on one |
| S4 gdext costs | Owner's Mac; hosted runners for CPU-side calls | Inside the 2 ms adapter split in Graphics and rendering | Spike 1's ladder |
| S5 `Mesher::<32>` plus the shaped pass on 3 hulls | Hosted macOS arm64 runner and the owner's Mac | 200 µs or less per chunk at p95 | Shaped faces build one frame after the greedy mesh. If p95 is still over, shaped cells beyond 300 m draw as full cells |
| S6 volume AO against vertex AO | Throttle profile; base M1 reports | Volume AO inside the opaque pass row on the Low tier | Plan B, vertex AO |
| S7 GPUParticles3D at 1k to 32k, and baker coverage of particle shaders | Throttle profile; benchmark reports | VFX pass row met at the tier's Layer B cap, and zero unbaked particle pipelines compiling at draw time | Lower the tier's Layer B cap until the row fits |

Milestone 0 gate: every spike above meets its gate or has applied its fallback, and the owner signs off on the spike report.

At the end of month 3, each spike that has not answered applies its fallbacks, and the milestone takes its one extension of up to 1 month. When the extension ends, any spike still unanswered keeps its fallback and Milestone 1 starts. Before Milestone 1 starts, two failures force a scope cut:

- **The minimum swarm misses after every fallback in Performance budgets.** Its floor drops in steps of 2 full-physics hostile hulls, never below 14, and capstone operations are authored to the new floor.
- **A client still exceeds the 768 kbps ceiling after spike 3's fallbacks.** The multiplayer projectile cap drops to 1,000, and every vessel replicates at 20 Hz.

### Milestone gates

Missed-gate rule for Milestone 1 onward: a milestone that misses its gate extends once, by 25% of its estimate. If the gate still fails, scope is cut by pillar priority, lowest row first (Pillars and non-goals), and the gate re-runs. Milestone 0 follows its own box instead.

**Vertical slice.** Act 1's first region is playable end to end, and playtest pre-releases start on GitHub Releases. The gate is all of these:

- The newcomer gates in The first ten minutes.
- The Quartermaster gets at least 18 of 20 briefings right.
- Gauges and splash meet their budgets in Live feedback and Instant testing, on the hosted runners and the throttle profile.
- The reference battle and co-op reference battle hold the 8 ms p99 tick with real content, and stay within the memory budgets.
- The first loop-length measurement in Core loop.
- The dusk console and chart palettes complete.
- The owner's sign-off.

At the vertical slice the project also moves to Godot 4.8 if three conditions hold: a 4.8.x stable release has been out for at least 30 days, it contains the Metal fence-timeout fix (issue 119436), and the full CI and frame-time gates pass on a 4.8 branch with `rendering/textures/streaming/enabled = false`. Otherwise it stays on 4.7.x and re-checks at each later milestone.

**First public release (0.1.0).** It ships the sandbox, the editor, skirmish, 2-player co-op, PvP elimination at 1v1 and 2v2, and Act 1 of the campaign. It also includes hull sonar, drones as the only offboard sensors, about 60 parts in 5 tiers, the high-contrast (7:1) palette, and full gamepad play of sorties, menus and the chart table. Multiplayer runs over UDP with automatic port mapping and join codes. Ships are shared as blueprint files and share strings. The gate is all of these:

- Act 1 content complete: about 20 enemy classes, 11 templates, 4 story operations, 2 ace arcs and the capstone.
- Act 1 campaign-bot balance passes for Alert, counter cards, template caps and the economy.
- Every Act 1 operation is winnable with stock officers on Normal, as a headless CI check.
- The loop-length gate re-runs on at least 200 playtest loops.
- 2-player co-op holds its tick on both runners and the throttle profile.
- At least 99% crash-free sessions in Sentry release health across the playtest pre-releases.
- At least 40% of playtest reporters who finish the tutorial complete 3 operations.
- The owner's sign-off.

L4 Luau scripting follows in the first update after it, under the fuzzing criterion in Scripting.

**Acts 2 to 4 (0.2.0 to 0.4.0).** Each act's row in Scope per act is complete. Act 2 adds enemy strike aircraft and the player helicopter deck. Act 3 adds AI-only enemy submarines and heavy guns of 130 to 155 mm. Act 4 adds the Open Theatre. Each act's gate is all of these:

- The act's new code fits inside the tick budget.
- Campaign-bot balance re-passes at that act's hull cap.
- At least one operation in the act needs slider or rule tuning, so stock officers lose it on Normal.
- At least 99.5% crash-free sessions.
- The owner's sign-off.

For Act 3, the first scope cut is submarines: the protected scope is four acts without them.

**1.0 (1.0.0).** The gate is all of these:

- 4-player co-op passes the co-op reference battle.
- PvP escort and team sizes of 3v3 and 4v4 are in.
- About 60 enemy classes exist across the four acts.
- The median full campaign is 40 hours or more, from playtest reports and the owner's runs. A campaign-bot run that scales measured loop times to all operations stands in under the sample rule.
- At least 99.5% crash-free sessions.
- The owner's sign-off.

Translations for 1.0 ship for whichever target languages have contributors. No language holds the gate.

### First public release

The first public release goes out free on GitHub Releases for macOS (arm64) and Windows (x86_64), as version 0.x with the scope above. The README and release notes describe what is in each build and promise no features or dates.

Steam is not a milestone. If the owner decides the game is really good, it goes to Steam for free at any point after the first public release. The owner pays the one-time $100 Steam Direct fee. The off-by-default `steam` feature then turns on the Steam Networking Sockets and relay transport and Steam Workshop. The Steam page promises no specific features or dates, which Valve's rules forbid, and any Steam early access answers about duration name gates, not dates ([Steam early access rules](https://partner.steamgames.com/doc/store/earlyaccess)). Until then the game does not go to Steam.

### Deferred past 1.0

Player-built aircraft and land vehicles, collaborative editing of one blueprint, code mods, on-foot play, and hosted ranked servers (the project runs no servers). Each one multiplies physics, AI, interface and netcode work, and Stormworks shipped its weapons a year after its 1.0 and still succeeded.

## Risks

The two risks most likely to sink the project are simulation performance and netcode for player-built ships, which is why Milestone 0 tests both before any content exists. Milestone 0 is a hard 3-month box with one extension of up to 1 month, and any spike still unanswered at the end applies its documented fallback. Scope creep in the campaign is the most likely way to ship late. With one builder, no owned Windows PC and zero cash spend, the risks most likely to reach players are Windows driver bugs and the friction of unsigned builds.

Every risk below has a decided mitigation. Where a mitigation depends on a measurement, the row states its gate and fallback; where another section owns them (Performance budgets, Graphics and rendering, Platform support, Summary), the row restates them.

| Risk | Why it is real | Mitigation |
| --- | --- | --- |
| The reference battle does not fit the budget | No published numbers cover hundreds of voxel hulls plus thousands of projectiles, and the project owns neither minimum-spec machine | Milestone 0 spikes 2 and 5 time the 8 ms p99 tick headless on the free hosted runners (Windows x64 with 4 vCPUs and 16 GB; macOS arm64 with a 3-core M1 and 7 GB, [GitHub-hosted runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)) and with rendering on the owner's Apple Silicon Pro/Max Mac under the throttle profile that approximates the base M1. Community testers report real minimum-spec results from the in-game benchmark scene. The degradation ladder in Performance budgets applies first; then the full-physics cap shrinks rather than the spec rising, never below the minimum swarm (20 full-physics hostile hulls, 8 drones, 6 enemy aircraft, 24 missiles in flight, 1,000 live projectiles) |
| Netcode retrofits | Stormworks reworked its multiplayer in 2022, 2023 and 2026 | One server-authoritative code path from day one, schema-generated replication, and bot soak tests over the UDP transport with a link conditioner in CI (spike 3: 150 ms latency, 5% packet loss) |
| Hosts cannot accept connections | No relay server is run (zero spend), so a host behind carrier-grade or symmetric NAT cannot be reached even when UPnP is present | The host maps its port automatically with UPnP IGD or NAT-PMP/PCP and shows the result; players join by address or by a join code that encodes address and port. If mapping fails, the README documents manual port forwarding and free mesh VPNs (Tailscale, ZeroTier). The Steam Networking Sockets and Steam Datagram Relay transport stays designed behind the transport trait and turns on with the Steam feature in phase 2 |
| No Windows GPU hardware, so Windows driver bugs reach players first | The project owns no Windows PC or GPU. Hosted Windows runners run Windows Server 2025, not consumer Windows 11, and have no GPU, so CI renders only through software rasterizers (Direct3D 12 WARP; Vulkan and OpenGL through Mesa lavapipe and llvmpipe where available), which catch API misuse but not vendor driver bugs. Windows is 93.70% of Steam users ([Steam Hardware Survey, October 2026](https://store.steampowered.com/hwsurvey/)) | Software-rendered golden captures under d3d12, vulkan and opengl3 on the hosted Windows runner; a logged fallback chain from Direct3D 12 to Vulkan to OpenGL 3; opt-in Sentry crash reports (free Developer plan) carrying GPU model, vendor, driver version, rendering driver and fallback history inside each event; benchmark-scene reports from community testers on Intel, AMD and NVIDIA GPUs. Every build goes out first as a GitHub prerelease. Gate: it is marked latest after 7 days with no new Windows crash signature in Sentry and no open rendering issue that reproduces on a Windows GPU. Fallback: the previous latest release stays latest until a fix passes the same gate |
| Graphics cost and Godot 4.7.2 Metal defects | No stack timings exist on the RTX 3050 6 GB or the base M1, and the project owns neither; the throttle profile on the owner's Mac only approximates the M1. Godot's Metal GPU timer is unusable in 4.7. The Metal fence-timeout freeze ([issue 119436](https://github.com/godotengine/godot/issues/119436)) is fixed only in 4.8 (PR 121392 raises the 1 s fence wait to 2 s), and the macOS shader baker crashes in some projects ([issue 124539](https://github.com/godotengine/godot/issues/124539)) | Graphics spikes S1 to S7 and gate T run before content work; each tier has a cut order; Mac gates are measured with Instruments or MTL\_HUD\_ENABLED=1 under the throttle profile, never with Godot's timer; community benchmark reports that miss a tier's frame target apply that tier's cut order in the next release. Baked Mac exports start from an empty shader cache in CI and ship unbaked behind a precompile screen on issue 124539 or a first frame over 10 s. Godot stays pinned at 4.7.2 by default. Gate (at the vertical slice and at each 4.8.x stable release after it): a branch on 4.8 with rendering/textures/streaming/enabled = false and a gdext release supporting api-4-8 passes the full CI matrix and every graphics gate, with no tier's p95 GPU time more than 5% above 4.7.2 in the reference-battle replay; the project then moves to 4.8. Fallback: it stays on 4.7.2, and if the fence freeze reproduces (a soak-test hang or any Sentry event carrying "timeout waiting for fence"), PR 121392 is cherry-picked into the custom export templates that CI already builds |
| Unsigned-build friction | Zero spend rules out notarization and Windows code signing. macOS builds are ad-hoc signed (codesign -s -), and macOS 15 and later removed the Control-click bypass. Windows SmartScreen warns on unsigned downloads, and Windows 11 Smart App Control, on by default only on clean installs, blocks unsigned apps ([SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)) | The README and every release note document first launch: on macOS, System Settings > Privacy & Security > Open Anyway; on Windows, SmartScreen's More info > Run anyway, and the fact that a machine with Smart App Control on cannot run the build. Signing stays a known limitation, also on Steam if the game goes there, unless the owner approves spending |
| Single-builder bus factor | Claude builds nearly all of the game, and the owner is the only reviewer and the main playtester. Context is lost between agent sessions, and owner review and playtest time gate every milestone | This doc is the build spec and lives in the repository; changes after the freeze are owner decisions recorded in it. Everything, art and audio included, is code in the public repository, so any session or contributor resumes from repository state alone. CI enforces the invariants (trial battery, Fit Check, lints, golden captures). Gate: every tagged release builds from a clean clone on the hosted runners with no manual step; a tag that fails is not released |
| Free services change their terms | The plan depends on free tiers: GitHub Actions on standard hosted runners (free and unlimited for public repositories), GitHub Releases and Pages, Sentry's free Developer plan and Hosted Weblate's free libre plan | Each sits behind one config or script. CI jobs are plain scripts, and the macOS jobs also run locally on the owner's Mac. Sentry is opt-in and the game runs without it. Translations live in the repository as files, so Weblate is a convenience, not the store of record |
| The campaign overruns | Nebulous: Fleet Command has shipped only Act 1 in over 4.5 years of early access; Sea Power's dynamic campaign has slipped | The first public release ships Act 1 only; later acts are data on shared systems; about 20 enemy classes at the first public release and about 60 by 1.0 |
| Version 0.x never reaches 1.0 | Sprocket is at 2.6 times its early access estimate and Flyout at 1.5 times so far ([Sprocket on Steam](https://store.steampowered.com/app/1674170/)) | 1.0 is defined by scope, not dates: Acts 1 to 4, about 60 enemy classes, and escort plus 3v3 and 4v4 PvP. Milestones are gated on playtest metrics. No dates are published on GitHub, and if the game goes to Steam its page promises no dates |
| Automation flattens ship design | When the defaults are good, every ship plays the same | Officers have documented limits that expert forks can beat. Every Act 1 operation is winnable with stock officers on Normal (a headless CI check); Acts 2 to 4 each include at least one operation that the defaults fail but presets, sliders or the rules table can pass. PvP has two rulesets: standard, the default, bans forked officers and graph edits; open allows both |
| Patent claim against adaptive enemies | Warner Bros.' US10926179B2 covers nemesis-style enemy memory, active to about 2036 ([Google Patents](https://patents.google.com/patent/US10926179B2/en)) | No enemy remembers individual player actions, ever. Adaptation stays at fleet-level doctrine cards, ace returns are authored and fixed, and hull records carry only fuel, ammunition, damage, veterancy and refits. No paid patent opinion is commissioned (zero spend). Fallback: if the project receives a written infringement claim, doctrine cards stop reacting to tallies and arrive on a fixed authored schedule per act |
| Dependency churn | gdext is 0.x with occasional breaking changes; renet2 reached 1.0 in October 2026 with one main maintainer | Exact pins with a committed Cargo.lock (godot =0.5.5 with api-4-7, rapier3d-f64 =0.36.1, renet2 =1.0.0, mlua =0.12.2); each dependency sits behind one crate or trait; upgrades land only behind the full CI matrix |
| Rapier voxel colliders are young | Voxels arrived in Rapier 0.25 in 2025; dynamic-body mass was never confirmed fixed | Cuboid compounds by default and game-computed mass. Voxels are adopted only if spike 2 shows them at least 20% faster in combined collider-rebuild and contact time over a 10-minute damage soak, with identical same-platform state hashes across 10 reruns |
| A guest's script escapes the sandbox on the host | Hosts run untrusted Luau from every guest | Scripts are off by default in every lobby and never run in public PvP. mlua is pinned at =0.12.2 with the luau feature only, so no JIT. Each vehicle VM has an 8 MB memory limit, 5,000 interrupts per graph tick and a 2 ms wall-clock backstop. The host API is fuzzed in CI with cargo-fuzz, and L4 scripts ship only after 30 consecutive days and 1,000 CPU-hours of fuzzing with zero escapes. A sandbox-escape fix ships as a hotfix within 72 hours of the upstream fix, or, if none exists, a hotfix that disables script hosting ships within 72 hours; other security fixes ship within 14 days |
| Title conflict | The title cleared only a knockout search on 12 November 2026, with no paid clearance opinion; Ubisoft's BLACK FLAG for games is close in meaning | The gate and fallback in the Summary: the search runs again before any Steam page, and if it finds a live identical mark for games in classes 9, 28 or 41, or the project receives a written claim, the game is renamed Wolfwater; the fiction names do not change |
| AI agents speed up less on non-code work | Agents help most with code and much less with physics tuning, feel, balance and content | Durations are estimates for an agent working continuously, gated by owner review and playtest time. Milestone gates measure the game, not the code volume. Tuning is turned into headless checks wherever it can be (trial battery, Fit Check, stock-officer winnability) |
| A direct competitor moves | Geometa has combat experience (Search and Destroy, Anymaker), and Sea Power targets 1.0 | Differentiate on the automatic editor, the lone-ship campaign, native macOS, and a free, open-source game |

## Sources

Three research rounds by Opus agents gathered these on 12 November 2026: design, UI and anti-slop, and graphics. Verifier agents re-checked each round against the pages it cites. A decision-freeze pass the same day checked pinned versions, platform behaviour and standards, and its sources are the last group below. Figures quoted from a source are as of that date. Sections also cite sources inline; this list groups the main ones by topic.

Three markers apply. "Not used" marks a source for an option the zero-spend plan dropped; it stays as the record of why the option was weighed. "Phase 2" marks a source that applies only if the game goes to Steam. "Single report" marks one user's or one third party's account: it can support a figure but never sets a gate on its own.

### Design round

- Engine and bindings: [Godot 4.6 release notes](https://godotengine.org/releases/4.6/), [Godot download archive](https://godotengine.org/download/archive/), [Godot thread-safe APIs](https://docs.godotengine.org/en/stable/tutorials/performance/thread_safe_apis.html), [Godot macOS export](https://docs.godotengine.org/en/stable/tutorials/export/exporting_for_macos.html), [Godot pipeline compilations](https://docs.godotengine.org/en/stable/tutorials/performance/pipeline_compilations.html), [godot-rust compatibility](https://godot-rust.github.io/book/toolchain/compatibility.html), [godot crate on crates.io](https://crates.io/api/v1/crates/godot)
- Simulation: [Rapier determinism guide](https://rapier.rs/docs/user_guides/rust/determinism), [parry3d Voxels](https://docs.rs/parry3d/latest/parry3d/shape/struct.Voxels.html), [rayon ParallelIterator](https://docs.rs/rayon/latest/rayon/iter/trait.ParallelIterator.html), [GPU Gems water simulation](https://developer.nvidia.com/gpugems/gpugems/part-i-natural-effects/chapter-1-effective-water-simulation-physical-models), [Teardown multiplayer and destruction](https://80.lv/articles/teardown-developer-breaks-down-multiplayer-and-voxel-destruction-tech)
- Networking: [Gaffer on Games snapshot interpolation](https://gafferongames.com/post/snapshot_interpolation/), [Gaffer on Games snapshot compression](https://gafferongames.com/post/snapshot_compression/), [ISteamNetworkingSockets](https://partner.steamgames.com/doc/api/ISteamNetworkingSockets) (phase 2 transport, behind the Steam feature), [renet2](https://github.com/UkoeHB/renet2) (phase 1 transport over UDP), [Besiege news](https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid=346010&count=500&maxlength=0&feeds=steam_community_announcements), [Space Engineers multiplayer overhaul](https://blog.marekrosa.org/2018/07/space-engineers-multiplayer-overhaul/)
- Scripting and services: [mlua Lua struct](https://docs.rs/mlua/latest/mlua/struct.Lua.html), [mlua changelog](https://github.com/mlua-rs/mlua/blob/main/CHANGELOG.md), [Luau performance](https://luau.org/performance), [Wasmtime deterministic execution](https://docs.wasmtime.dev/examples-deterministic-wasm-execution.html), [Steam auth](https://partner.steamgames.com/doc/features/auth) (phase 2), [Steam Workshop implementation](https://partner.steamgames.com/doc/features/workshop/implementation) (phase 2), [Fireworks AI acquires Hathora](https://fireworks.ai/blog/fireworks-acquires-hathora) (hosted servers, not used)
- Windows and macOS: [GitHub larger runners](https://docs.github.com/en/actions/reference/runners/larger-runners) (not used; CI runs on standard hosted runners), [GitHub runner images](https://github.com/actions/runner-images), [Artifact Signing quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart) (not used; Windows builds ship unsigned), [Artifact Signing FAQ](https://learn.microsoft.com/en-us/azure/artifact-signing/faq) (not used), [timeBeginPeriod](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod), [Windows path limits](https://learn.microsoft.com/en-us/windows/win32/fileio/maximum-file-path-limitation), [Rust linkage](https://doc.rust-lang.org/reference/linkage.html), [Embark crash-handling](https://github.com/EmbarkStudios/crash-handling)
- Genre and design: [Stormworks on Steam](https://store.steampowered.com/app/573090/Stormworks_Build_and_Rescue/), [Dan Walters on Stormworks](https://filmstories.co.uk/features/build-it-and-they-will-come-designer-dan-walters-on-the-future-of-stormworks/), [From the Depths on Steam](https://store.steampowered.com/app/268650/From_the_Depths/), [Ultimate Admiral: Dreadnoughts on Steam](https://store.steampowered.com/app/1069660/Ultimate_Admiral_Dreadnoughts/), [Space Engineers grid AI guide](https://spaceengineersgame.com/new-players/grid-ai-guide), [Command weapon release authorization](https://command.matrixgames.com/?&p=3598), [From the Depths detection](https://fromthedepths.wiki.gg/wiki/Detection), [Sea Power on Steam](https://store.steampowered.com/app/1286220/Sea_Power/), [NEBULOUS on Steam](https://store.steampowered.com/app/887570/NEBULOUS_Fleet_Command/), [HighFleet](https://en.wikipedia.org/wiki/HighFleet), [Carrier Command 2](https://en.wikipedia.org/wiki/Carrier_Command_2), [FTL](https://en.wikipedia.org/wiki/FTL:_Faster_Than_Light), [Flyout on Steam](https://store.steampowered.com/app/777390/Flyout), [NavalArt on Steam](https://store.steampowered.com/app/842780/NavalArt)

### UI and anti-slop round

- Generic-design tells: [Anthropic frontend-design skill](https://raw.githubusercontent.com/anthropics/skills/main/skills/frontend-design/SKILL.md) (eyebrows, middle-dot meta strings, decorative monospace labels), [Impeccable PR 129](https://github.com/pbakaus/impeccable/pull/129) (hero-eyebrow-chip lint), [Krebs on design slop](https://www.adriankrebs.ch/blog/design-slop/) (1,590 Show HN sites), [Hacker News on all-caps eyebrows](https://news.ycombinator.com/item?id=49765151), [Hacker News on numbered sections in game launch sites](https://news.ycombinator.com/item?id=50068311), [Material 2 to Material 3 type mapping](https://developer.android.com/develop/ui/compose/designsystems/material2-material3) (overline into labelSmall)
- Accessibility and capture checks: [Xbox Accessibility Guideline 101](https://learn.microsoft.com/en-us/gaming/accessibility/xbox-accessibility-guidelines/101) (text size), [EA Fonttik](https://github.com/electronicarts/fonttik) (text size and contrast on captures)

### Graphics round

- Art direction: [Teardown frame teardown](https://acko.net/blog/teardown-frame-teardown/), [Voxagon, from screen space to voxel space](https://blog.voxagon.se/2018/10/17/from-screen-space-to-voxel-space.html), [0fps voxel ambient occlusion](https://0fps.net/2013/07/03/ambient-occlusion-for-minecraft-like-worlds/), [The technical art of Sea of Thieves](https://history.siggraph.org/wp-content/uploads/2022/09/2018-Talks-Ang_The-Technical-Art-of-Sea-of-Thieves.pdf), [Sable readability](https://www.gamedeveloper.com/marketing/how-shedworks-refined-the-art-of-sable-in-pursuit-of-readability), [Lonely Mountains: Downhill production](https://80.lv/articles/level-game-production-lonely-mountains-downhill), [Albrecht, how to make low poly look good](https://www.gamedeveloper.com/design/how-to-make-low-poly-look-good), [Lucas Pope on Obra Dinn's art style](https://blog.playstation.com/archive/2019/10/17/lucas-pope-on-return-of-the-obra-dinns-art-style), [The art of Firewatch](https://www.thumbsticks.com/gdc-2015-the-art-of-firewatch) (secondary report), [How Townscaper works](https://www.gamedeveloper.com/game-platforms/how-townscaper-works-a-story-four-games-in-the-making), [USN WWII ship camouflage measures](https://en.wikipedia.org/wiki/World_War_II_ship_camouflage_measures_of_the_United_States_Navy) (paint swatches), [Babylon.js bevelled cube approaches](https://forum.babylonjs.com/t/approaches-for-bevel-rounded-corner-cubes-at-render-time/5771) (single report), [Stormworks negative reviews](https://steamcommunity.com/app/573090/negativereviews/?browsefilter=toprated) (10 sampled; single report), [Stormworks bloom thread](https://steamcommunity.com/app/573090/discussions/0/1694922526903721131) (single report)
- Godot releases and benchmarks: [Godot 4.4 release notes](https://godotengine.org/releases/4.4/), [Godot 4.5 release notes](https://godotengine.org/releases/4.5/), [Godot 4.7 release notes](https://godotengine.org/releases/4.7/), [Godot benchmarks](https://benchmarks.godotengine.org/), [benchmark rig and settings](https://github.com/godotengine/godot-benchmarks/blob/main/web/layouts/index.html), [benchmark run of 10 November 2026](https://github.com/godotengine/godot-benchmarks-results/blob/main/2026-11-10_e6e2a8742.md) (RX 550 Sponza pass costs), [benchmark run of 7 July 2025](https://github.com/godotengine/godot-benchmarks-results/blob/main/2025-07-07_4d1f26e1f.md) (SSR before the overhaul), [Sponza benchmark script](https://github.com/godotengine/godot-benchmarks/blob/main/benchmarks/rendering/sponza.gd)
- Godot rendering and post: [Environment class, 4.7](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/Environment.xml), [environment and post-processing](https://docs.godotengine.org/en/4.7/tutorials/3d/environment_and_post_processing.html), [3D antialiasing](https://docs.godotengine.org/en/4.7/tutorials/3d/3d_antialiasing.html), [renderers](https://docs.godotengine.org/en/4.7/tutorials/rendering/renderers.html), [Forward+ scene shader](https://raw.githubusercontent.com/godotengine/godot/4.7-stable/servers/rendering/renderer_rd/shaders/forward_clustered/scene_forward_clustered.glsl) (fog formula), [Compatibility tonemapper](https://raw.githubusercontent.com/godotengine/godot/4.7-stable/drivers/gles3/shaders/tonemap_inc.glsl), [renderer_viewport.cpp](https://github.com/godotengine/godot/blob/4.7-stable/servers/rendering/renderer_viewport.cpp) (upscaler fallbacks), [Viewport class](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/Viewport.xml), [ProjectSettings class](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/ProjectSettings.xml), [RenderingServer class](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/RenderingServer.xml), [Light3D class](https://raw.githubusercontent.com/godotengine/godot/4.7-stable/doc/classes/Light3D.xml), [CameraAttributes class](https://github.com/godotengine/godot/blob/4.7/doc/classes/CameraAttributes.xml), [CompositorEffect class](https://github.com/godotengine/godot/blob/4.7/doc/classes/CompositorEffect.xml), [global illumination](https://docs.godotengine.org/en/4.7/tutorials/3d/global_illumination/introduction_to_global_illumination.html), [HDR output](https://docs.godotengine.org/en/4.7/tutorials/rendering/hdr_output.html), [lights and shadows](https://docs.godotengine.org/en/4.7/tutorials/3d/lights_and_shadows.html), [Mobile renderer limits](https://github.com/godotengine/godot/blob/4.7/servers/rendering/renderer_rd/forward_mobile/render_forward_mobile.h), [RD mesh storage](https://github.com/godotengine/godot/blob/4.7/servers/rendering/renderer_rd/storage_rd/mesh_storage.cpp) (MultiMesh motion vectors), [GLES3 mesh storage](https://github.com/godotengine/godot/blob/4.7/drivers/gles3/storage/mesh_storage.cpp), [D3D12 driver](https://github.com/godotengine/godot/blob/4.7/drivers/d3d12/rendering_device_driver_d3d12.cpp) (no pipeline cache), [PR 124378](https://github.com/godotengine/godot/pull/124378) (MSAA resolve, 4.8), [PR 102330](https://github.com/godotengine/godot/pull/102330) (SMAA 1x), [PR 99603](https://github.com/godotengine/godot/pull/99603) (MetalFX temporal on an M1 Max), [Godot-Motion-Blur-Addon](https://github.com/sphynx-owner/Godot-Motion-Blur-Addon), [reverse-Z depth](https://godotengine.org/article/introducing-reverse-z/), [internal rendering architecture](https://github.com/godotengine/godot-docs/blob/master/engine_details/architecture/internal_rendering_architecture.rst) (Metal 4 on macOS 26)
- Godot defects: [issue 123125](https://github.com/godotengine/godot/issues/123125) (D3D12 against Vulkan), [issue 123266](https://github.com/godotengine/godot/issues/123266) (baked macOS exports below macOS 14), [issue 124539](https://github.com/godotengine/godot/issues/124539) (macOS shader baker crash), [issue 119436](https://github.com/godotengine/godot/issues/119436) (Metal fence timeout), [issue 125225](https://github.com/godotengine/godot/issues/125225) (Metal subpasses on Mobile), [issue 125124](https://github.com/godotengine/godot/issues/125124) (Metal GPU timer against Instruments), [issue 114258](https://github.com/godotengine/godot/issues/114258) (Metal process time; single report), [issue 92254](https://github.com/godotengine/godot/issues/92254) (TAA under MoltenVK), [issue 103723](https://github.com/godotengine/godot/issues/103723) (MacBook Air M1 regression), [Metal driver timestamps](https://github.com/godotengine/godot/blob/4.7-stable/drivers/metal/rendering_device_driver_metal.cpp), [Metal device properties](https://github.com/godotengine/godot/blob/4.7-stable/drivers/metal/metal_device_properties.cpp), [dynamic resolution proposal 917](https://github.com/godotengine/godot-proposals/issues/917)
- Ocean and sky: [TIME in renderer_compositor_rd.cpp](https://github.com/godotengine/godot/blob/4.7-stable/servers/rendering/renderer_rd/renderer_compositor_rd.cpp), [Forward+ alpha-pass rules](https://github.com/godotengine/godot/blob/4.7-stable/servers/rendering/renderer_rd/forward_clustered/scene_shader_forward_clustered.h), [spatial shaders](https://docs.godotengine.org/en/4.7/tutorials/shaders/shader_reference/spatial_shader.html), [spatial shader source docs](https://github.com/godotengine/godot-docs/blob/master/tutorials/shaders/shader_reference/spatial_shader.rst) (stencil), [volumetric fog](https://docs.godotengine.org/en/4.7/tutorials/3d/volumetric_fog.html), [sky shaders](https://docs.godotengine.org/en/4.7/tutorials/shaders/shader_reference/sky_shader.html), [PhysicalSkyMaterial](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/PhysicalSkyMaterial.xml), [GodotOceanWaves](https://github.com/2Retr0/GodotOceanWaves), [GodotOceanWaves on 4.5 and later](https://forum.godotengine.org/t/godot-4-3-water-shader-to-4-4/137925?page=2), [godot4-oceanfft](https://github.com/tessarakkt/godot4-oceanfft), [OceanPro 1.0](https://forum.godotengine.org/t/oceanpro-1-0-gpu-fft-ocean-system-for-godot-4-7/144004), [Atlas interactive water](https://gpuopen.com/gdc-presentations/2019/gdc-2019-agtd6-interactive-water-simulation-in-atlas.pdf), [rtryan98 ocean rendering part 2](https://rtryan98.github.io/2026/03/22/ocean-rendering-part-2.html), [Unity HDRP water](https://unity.com/blog/engine-platform/new-hdrp-water-system-in-2022-lts-and-2023-1), [Crest clipping](https://crest.readthedocs.io/en/stable/user/clipping.html), [Tessendorf course notes](https://people.computing.clemson.edu/~jtessen/reports/papers_files/coursenotes2004.pdf), [Pierson-Moskowitz spectrum](https://geo.libretexts.org/Bookshelves/Oceanography/Introduction_to_Physical_Oceanography_%28Stewart%29/16:_Ocean_Waves/16.4:_Ocean-Wave_Spectra), [sea state](https://en.wikipedia.org/wiki/Sea_state), [Kelvin wake pattern](https://en.wikipedia.org/wiki/Kelvin_wake_pattern), [GPU Gems 2 geometry clipmaps](https://developer.nvidia.com/gpugems/gpugems2/part-i-geometric-complexity/chapter-2-terrain-rendering-using-gpu-based-geometry), [volumetric cloud demo v2](https://github.com/clayjohn/godot-volumetric-cloud-demo-v2), [StraySpark on 4.6 SSR](https://www.strayspark.studio/blog/godot-46-rendering-deep-dive-ssr-lightmapper-performance) (single report)
- VFX: [GPUParticles3D class, 4.7.2](https://github.com/godotengine/godot/blob/4.7.2-stable/doc/classes/GPUParticles3D.xml), [particles storage, view-depth sort](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_rd/storage_rd/particles_storage.cpp#L1236-L1331), [particles storage, fixed-fps stepping](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_rd/storage_rd/particles_storage.cpp#L1469-L1625), [particles storage, emission cap](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_rd/storage_rd/particles_storage.cpp#L604-L642), [sub-emitters in particles.glsl](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_rd/shaders/particles.glsl#L197-L216), [particle culling in renderer_scene_cull.cpp](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_scene_cull.cpp#L2993-L3005), [shader baker export plugin](https://github.com/godotengine/godot/blob/4.7.2-stable/editor/export/shader_baker_export_plugin.cpp#L277-L292), [MultiMesh culling in the Forward+ shader](https://github.com/godotengine/godot/blob/4.7.2-stable/servers/rendering/renderer_rd/shaders/forward_clustered/scene_forward_clustered.glsl#L777-L846), [particle collision](https://docs.godotengine.org/en/stable/tutorials/3d/particles/collision.html), [particle turbulence](https://docs.godotengine.org/en/stable/tutorials/3d/particles/turbulence.html), [decals](https://docs.godotengine.org/en/stable/tutorials/3d/using_decals.html), [StandardMaterial3D](https://docs.godotengine.org/en/stable/tutorials/3d/standard_material_3d.html), [GPU optimization](https://docs.godotengine.org/en/stable/tutorials/performance/gpu_optimization.html), [GPUParticles3D frame-drop report](https://forum.godotengine.org/t/gpu-particle3d-fps-drop/143606) (single report), [issue 87891](https://github.com/godotengine/godot/issues/87891) (first-emission stutter), [issue 114900](https://github.com/godotengine/godot/issues/114900) (one-shot emitters on M1), [issue 94136](https://github.com/godotengine/godot/issues/94136) (trail flicker), [Apple tile-based deferred rendering, WWDC20 10631](https://developer.apple.com/videos/play/wwdc2020/10631/), [Jedi: Fallen Order splashes](https://simonschreibt.de/gat/jedi-fallen-order-splishy-splashy/), [League of Legends VFX tips](https://www.vfxapprentice.com/blog/10-league-of-legends-vfx-design-tips) (secondary report)
- Ship meshing: [binary-greedy-meshing](https://github.com/Inspirateur/binary-greedy-meshing), [binary-greedy-meshing source](https://github.com/Inspirateur/binary-greedy-meshing/blob/main/src/lib.rs), [Binary Greedy Meshing v2 in C++](https://github.com/cgerikj/binary-greedy-meshing), [block-mesh-rs](https://github.com/bonsairobo/block-mesh-rs), [block-mesh on crates.io](https://crates.io/crates/block-mesh) (unmaintained), [fast-surface-nets-rs](https://github.com/bonsairobo/fast-surface-nets-rs), [0fps meshing in a Minecraft game](https://0fps.net/2012/06/30/meshing-in-a-minecraft-game/), [godot_voxel blocky terrain](https://voxel-tools.readthedocs.io/en/latest/blocky_terrain/), [godot_voxel performance](https://voxel-tools.readthedocs.io/en/latest/performance/), [godot_voxel issue 150](https://github.com/Zylann/godot_voxel/issues/150), [Veloren greedy.rs](https://docs.veloren.net/src/veloren_voxygen/mesh/greedy.rs.html), [Veloren devblog 39](https://veloren.net/blog/devblog-39/), [Teardown on Software Engineering Daily](https://softwareengineeringdaily.com/wp-content/uploads/2024/12/SED1772-Teardown.txt), [Voxagon 2024 summary](https://blog.voxagon.se/2024/12/29/year-summary.html), [surface byte layout in rendering_server.cpp](https://github.com/godotengine/godot/blob/4.7/servers/rendering/rendering_server.cpp), [RenderingDevice class](https://raw.githubusercontent.com/godotengine/godot/4.7/doc/classes/RenderingDevice.xml) (texture_3d_update), [MultiMesh class](https://raw.githubusercontent.com/godotengine/godot/4.7/doc/classes/MultiMesh.xml), [ArrayMesh class](https://raw.githubusercontent.com/godotengine/godot/4.7/doc/classes/ArrayMesh.xml), [shading language](https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/shading_language.html), [visibility ranges](https://docs.godotengine.org/en/4.6/tutorials/3d/visibility_ranges.html), [optimizing 3D performance](https://docs.godotengine.org/en/4.6/tutorials/performance/optimizing_3d_performance.html), [Space Engineers VRAGE render plan](https://blog.marekrosa.org/2016/08/guest-post-by-jan-hlousek-vrage-render_25/)
- Tiers and hardware: [graphics settings demo](https://github.com/godotengine/godot-demo-projects/blob/master/3d/graphics_settings/settings.gd), [TPS demo settings](https://github.com/godotengine/tps-demo/blob/master/menu/settings.gd), [OS class](https://github.com/godotengine/godot/blob/4.7-stable/doc/classes/OS.xml) (device info), [FidelityFX FSR 2](https://github.com/GPUOpen-Effects/FidelityFX-FSR2), [Apple M1](https://en.wikipedia.org/wiki/Apple_M1), [GeForce 20 series](https://en.wikipedia.org/wiki/GeForce_20_series), [GeForce RTX 30 series](https://en.wikipedia.org/wiki/GeForce_RTX_30_series), [GeForce RTX 40 series](https://en.wikipedia.org/wiki/GeForce_RTX_40_series), [RTX 2060 specifications](https://technical.city/en/video/GeForce-RTX-2060-vs-Apple-M1-8-Core-GPU), [3DMark on the MacBook Air M1](https://benchmarks.ul.com/hardware/tablet/MacBook+Air+%28M1,+2020%29+review), [M1 Air throttling](https://forums.macrumors.com/threads/m1-air-throttles-to-a-maximum-of-4w-during-a-game.2305984/) (single report), [recommendedMaxWorkingSetSize](https://github.com/ggml-org/llama.cpp/issues/2048), [ProcessInfo.ThermalState](https://developer.apple.com/documentation/foundation/processinfo/thermalstate-swift.enum), [Mac model identifiers](https://support.apple.com/en-ca/HT201862), [Steam Mac chips, December 2025](https://www.mactrast.com/2026/01/m4-is-the-top-apple-silicon-chip-among-mac-gamers-on-steam/amp/) (secondary report)

### Decision freeze

- Toolchain and crates: [godot 0.5.5](https://crates.io/crates/godot/0.5.5), [gdext changelog](https://github.com/godot-rust/gdext/blob/master/Changelog.md), [rapier3d-f64 0.36.1](https://crates.io/crates/rapier3d-f64/0.36.1), [Rapier changelog](https://github.com/dimforge/rapier/blob/master/CHANGELOG.md), [renet2_steam 1.0.0](https://crates.io/crates/renet2_steam/1.0.0) (phase 2), [mlua versions](https://crates.io/api/v1/crates/mlua/versions), [Rust releases](https://github.com/rust-lang/rust/blob/master/RELEASES.md), [Rust float semantics, RFC 3514](https://rust-lang.github.io/rfcs/3514-float-semantics.html), [pathfinding astar](https://docs.rs/pathfinding/4.16.0/pathfinding/directed/astar/fn.astar.html), [dubins_paths](https://crates.io/crates/dubins_paths), [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz), [GdUnit4 6.2.3](https://github.com/godot-gdunit-labs/gdUnit4/releases/tag/v6.2.3), [gdUnit4-action](https://github.com/godot-gdunit-labs/gdUnit4-action)
- Platform behaviour: [Godot 4.7 system requirements](https://docs.godotengine.org/en/4.7/about/system_requirements.html), [Godot 4.7.2 macOS export plugin](https://github.com/godotengine/godot/blob/4.7.2-stable/platform/macos/export/export_plugin.cpp), [Godot 4.7.2 Windows manifest](https://github.com/godotengine/godot/blob/4.7.2-stable/platform/windows/godot.manifest) (longPathAware), [Godot 4.7.2 AccessibilityServer](https://raw.githubusercontent.com/godotengine/godot/4.7.2-stable/doc/classes/AccessibilityServer.xml), [SetProcessInformation](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-setprocessinformation), [CreateWaitableTimerExW](https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-createwaitabletimerexw), [Windows 11 supported Intel processors](https://learn.microsoft.com/en-us/windows-hardware/design/minimum/supported/windows-11-supported-intel-processors), [Windows 11 supported AMD processors](https://learn.microsoft.com/en-us/windows-hardware/design/minimum/supported/windows-11-supported-amd-processors), [SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation), [GitHub Actions runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing) (standard runners free for public repositories), [Steam Runtime README](https://github.com/ValveSoftware/steam-runtime/blob/master/README.md) (Linux server build container)
- Crash reports and privacy: [Sentry pricing](https://sentry.io/pricing/) (Developer plan, $0), [Sentry Godot options](https://docs.sentry.io/platforms/godot/configuration/options/), [Sentry Godot SDK 2.5.1](https://github.com/getsentry/sentry-godot/releases/tag/2.5.1), [sentry-rust minidump](https://github.com/getsentry/sentry-rust/blob/0.49.5/sentry-minidump/src/lib.rs), [EDPB Guidelines 2/2023](https://www.edpb.europa.eu/system/files/2024-10/edpb_guidelines_202302_technical_scope_art_53_eprivacydirective_v2_en_0.pdf), [CJEU C-673/17, Planet49](https://curia.europa.eu/juris/liste.jsf?num=C-673/17), [UK Data (Use and Access) Act 2025, schedule 12](https://www.legislation.gov.uk/ukpga/2025/18/schedule/12)
- UI assets and standards: [NOAA Chart 18649](https://historicalcharts.noaa.gov/pdfs/18649.pdf) (chart-table palette), [NOAA chart FAQ](https://nauticalcharts.noaa.gov/learn/faq.html) (reuse terms), [US Chart No. 1](https://nauticalcharts.noaa.gov/publications/us-chart-1.html), [US government works](https://www.usa.gov/government-copyright), [Atkinson Hyperlegible Next](https://github.com/googlefonts/atkinson-hyperlegible-next), [Atkinson Hyperlegible Mono](https://github.com/googlefonts/atkinson-hyperlegible-next-mono), [WCAG 2.2 non-text contrast](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html), [IMO MSC.302(87) bridge alert management](https://wwwcdn.imo.org/localresources/en/KnowledgeCentre/IndexofIMOResolutions/MSCResolutions/MSC.302%2887%29.pdf), [Blender trackpad input](https://docs.blender.org/manual/en/latest/editors/preferences/input.html)
- Flooding: [Ruponen et al. 2022 flooding benchmark](https://doi.org/10.1016/j.apor.2022.103153), [open copy](https://acris.aalto.fi/ws/portalfiles/portal/81724725/1_s2.0_S014111872200102X_main.pdf)
- Steam, phase 2: [Steam Content Survey](https://partner.steamgames.com/doc/gettingstarted/contentsurvey?l=english), [Steam community moderation](https://partner.steamgames.com/doc/marketing/community_moderation?l=english), [EU Digital Services Act text](https://eur-lex.europa.eu/legal-content/EN/TXT/HTML/?uri=CELEX:32022R2065), [Steam Playtest](https://partner.steamgames.com/doc/features/playtest)
- Not used under zero spend: [Apple notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) (macOS builds are ad-hoc signed), [Artifact Signing pricing](https://azure.microsoft.com/en-us/pricing/details/artifact-signing/), [AWS NVIDIA drivers](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/install-nvidia-driver.html) (no cloud GPU runner)

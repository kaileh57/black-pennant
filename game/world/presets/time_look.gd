class_name WorldTimeLook
extends Resource
## The part of a time-of-day preset that is not a colour: how the sky shader draws the sun, stars, moon,
## cirrus and the band below the horizon. Colours come from res://art/tokens/world_tokens.json.
## DESIGN.md L1868-1876, L1913. Unit: world-scene. One resource per Bp.TIME_* word in this folder.

## Overall sky brightness multiplier.
@export_range(0.0, 4.0) var sky_energy: float = 1.0
## How tightly the horizon colour hugs the horizon (larger = thinner band).
@export_range(1.0, 16.0) var horizon_curve: float = 7.0
## Warm scatter around the sun. 0 at night: the sun is below the horizon and only the moon lights the sky.
@export_range(0.0, 2.0) var sun_glow: float = 1.0
## Sun disc brightness as a multiple of the light's colour x energy (the moon disc uses it too).
@export_range(0.0, 32.0) var sun_disc: float = 8.0
## Star brightness; 0 by day and dusk.
@export_range(0.0, 4.0) var star_energy: float = 0.0
## How far the colour below the horizon (seen only by reflections and ambient) lifts from sea.deep towards
## the horizon colour.
@export_range(0.0, 1.0) var ground_lift: float = 0.35
## How far cirrus moves from the horizon colour towards the key light colour (by night it is the horizon
## colour scaled by cirrus_night_gain instead).
@export_range(0.0, 1.0) var cirrus_key_mix: float = 0.6
@export_range(0.0, 4.0) var cirrus_night_gain: float = 0.0
## Elevation of the key light used where no simulation supplies one (dry dock, warm-up), degrees.
@export_range(-90.0, 90.0) var static_sun_elevation_deg: float = 12.0

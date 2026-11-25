class_name WorldWeatherLook
extends Resource
## The part of a weather preset that is not a colour: sky layers, shadow softness and the static fog
## numbers the dry dock and the warm-up scene use when no simulation supplies them. Colours, light
## energies, ambient and exposure come from res://art/tokens/world_tokens.json ("presets"), never from here.
## DESIGN.md L1857-1883 (weather table), L1913-1915 (sky). Unit: world-scene.
## One resource per weather in this folder, named after the Bp.WEATHER_* word.

## Full cloud deck: 0 clear sky .. 1 one grey layer that hides the sun disc, stars and cirrus.
@export_range(0.0, 1.0) var overcast: float = 0.0
## Banded cirrus cover in the sky shader, 0..1 (scattered masses are mesh clouds drawn by Rust).
@export_range(0.0, 1.0) var cirrus_cover: float = 0.0
## Rain shafts under the deck, scaled by the live weather's rain amount.
@export_range(0.0, 1.0) var rain_shafts: float = 0.0
## Multiplies the time of day's sun scatter in the sky.
@export_range(0.0, 2.0) var sun_glow: float = 1.0
## Shadow opacity of the key light: diffuse daylight under cloud casts paler shadows.
@export_range(0.0, 1.0) var shadow_opacity: float = 1.0
## Share of the ambient light taken from the sky's own radiance rather than the preset's ambient colour
## (the rest); capped per preset so ambient never falls under the token floor.
@export_range(0.0, 1.0) var ambient_sky_share: float = 0.6
## Aerial perspective of the weather table (L1859-1866), for scenes with no simulation.
@export_range(0.0, 1.0) var aerial_perspective: float = 0.7

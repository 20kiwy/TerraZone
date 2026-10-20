# tz-rail.json — the Tanzania rail atlas

The railway network the SGR Trains feed rides and the map highlights: **every** railway line
OSM has for Tanzania (the central line, the SGR, the TAZARA, the branches — 2 441 polylines)
and the named railway stations and halts (185 points).

**Provenance:** extracted from the Overpass API (maps.mail.ru mirror) — every Tanzania railway
way (`railway=rail|narrow_gauge|light_rail|tram|construction|disused|abandoned|preserved|monorail`)
with its tags, clipped to the Tanzania boundary relation, classified **by name and gauge**
(the SGR: names ending "SGR" / `gauge=1435` / `electrified=contact_line`; the TAZARA:
`gauge=1067`; the old Central Line: `gauge=1000` / "Central Line"), healed along the corridor
(unnamed ways within 80 m of a named SGR way reclassify), simplified with Douglas-Peucker at a
10 m tolerance, rounded to 5 decimals. Stations merge the 0.16.0 GeoFabrik points (the SGR
corridor's own) with a fresh Overpass node sweep. Rebuild: the tiled Overpass fetch + the
classification pass recorded in the 0.26.0 worklog.

**Shape:** `{"lines": [{"cls":"sgr|tazara|central|branch","railway":"rail","name":"…",
"gauge":"1435","electrified":"contact_line","flat":[lon,lat,…]}, …],
"stations": [[name, lon, lat], …]}` — one metadata object per line (the old bare-number
form still parses as a Branch line), parsed by `terrazon_fleet::RailAtlas::parse`.

**License:** the data is © OpenStreetMap contributors, ODbL 1.0 — the NOTICE file carries the
attribution, and any redistribution of this atlas keeps it.

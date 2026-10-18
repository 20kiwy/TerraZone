# tz-rail.json — the Tanzania rail atlas

The railway network the SGR Trains feed rides and the map highlights: **every** railway line
OSM has for Tanzania (the central line, the SGR, the TAZARA, the branches — 2 441 polylines)
and the named railway stations and halts (185 points).

**Provenance:** extracted from GeoFabrik's Tanzania OSM extract (`tanzania-latest-free.shp.zip`,
layers `gis_osm_railways_free_1` and `gis_osm_transport_free_1`, fclass rail / narrow_gauge /
railway_station / railway_halt), simplified with Douglas-Peucker at a 10 m tolerance, rounded
to 5 decimals. The generator is the shapefile reader documented in `crates/map/src/shapefile.rs`
plus the DP pass recorded in the 0.16.0 worklog; re-run it to refresh the atlas.

**Shape:** `{"lines": [[lon,lat,lon,lat,…], …], "stations": [[name, lon, lat], …]}` — one flat
array per line, parsed by `terrazon_fleet::RailAtlas::parse`.

**License:** the data is © OpenStreetMap contributors, ODbL 1.0 — the NOTICE file carries the
attribution, and any redistribution of this atlas keeps it.

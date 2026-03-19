# Netan API Examples

These examples assume you already imported an Ile-de-France dataset, for example:

```bash
cargo run -p netan-cli -- dataset import datasets/ile-de-france-latest.osm.pbf --name ile_de_france_2026_03
```

Start the API with one default profile and one additional selectable profile:

```bash
cargo run -p netan-cli -- api serve \
  --dataset ile_de_france_2026_03 \
  --default-profile examples/profiles/car_research_v1.yml \
  --profile examples/profiles/pedestrian_research_v1.yml \
  --bind 127.0.0.1:8080
```

Inspect the loaded dataset, available profiles, and supported options:

```bash
curl http://127.0.0.1:8080/v1/service
curl http://127.0.0.1:8080/v1/profiles
```

Run a car route from Bastille to La Defense:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_car.json
```

Run a pedestrian route from Jardin du Luxembourg to Notre-Dame:

```bash
curl -X POST http://127.0.0.1:8080/v1/route \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_route_pedestrian.json
```

Run an OD batch:

```bash
curl -X POST http://127.0.0.1:8080/v1/od \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_od.json
```

Run a small matrix:

```bash
curl -X POST http://127.0.0.1:8080/v1/matrix \
  -H 'content-type: application/json' \
  --data @examples/api/ile_de_france_matrix.json
```

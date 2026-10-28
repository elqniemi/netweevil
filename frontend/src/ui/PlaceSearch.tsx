import { useEffect, useRef, useState } from "react";
import type { ServiceInfo } from "../api/types";
import { flyTo, getState, nextPointId, setRouteEnd, setState, useStore, type RouteEnd } from "../state/store";
import { TOOL_BY_ID } from "../tools/registry";

interface Place {
  label: string;
  lon: number;
  lat: number;
}

const NOMINATIM = "https://nominatim.openstreetmap.org/search";

/**
 * Header search box: a place name (OpenStreetMap Nominatim, limited to the
 * dataset extent) or a "lon, lat" pair. Results fly the map there, or set the
 * active route's A or B.
 */
export function PlaceSearch({ service }: { service: ServiceInfo | null }) {
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<Place[]>([]);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const tool = useStore((s) => s.tool);
  const routeTool = TOOL_BY_ID[tool].input === "routes";
  const timer = useRef<number | null>(null);
  const boxRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, []);

  useEffect(() => {
    if (timer.current) clearTimeout(timer.current);
    const q = query.trim();
    if (q.length < 3) {
      setResults([]);
      return;
    }
    const coords = q.match(/^(-?\d+(?:\.\d+)?)[ ,;]+(-?\d+(?:\.\d+)?)$/);
    if (coords) {
      setResults([{ label: `${coords[1]}, ${coords[2]}`, lon: Number(coords[1]), lat: Number(coords[2]) }]);
      setOpen(true);
      return;
    }
    timer.current = window.setTimeout(async () => {
      setBusy(true);
      try {
        const b = service?.dataset.topology_bounds;
        const params = new URLSearchParams({ q, format: "jsonv2", limit: "6" });
        if (b) {
          params.set("viewbox", `${b.min_lon},${b.max_lat},${b.max_lon},${b.min_lat}`);
          params.set("bounded", "1");
        }
        const response = await fetch(`${NOMINATIM}?${params}`, { headers: { accept: "application/json" } });
        const data = (await response.json()) as { display_name: string; lon: string; lat: string }[];
        setResults(data.map((d) => ({ label: d.display_name, lon: Number(d.lon), lat: Number(d.lat) })));
        setOpen(true);
      } catch {
        setResults([]);
      } finally {
        setBusy(false);
      }
    }, 350);
  }, [query, service]);

  const go = (place: Place) => {
    flyTo(place.lon, place.lat, 14);
    setOpen(false);
  };

  const place = (target: RouteEnd, p: Place) => {
    const s = getState();
    setRouteEnd(Math.min(s.activeRoute, s.routes.length - 1), target, { id: nextPointId(target), lon: p.lon, lat: p.lat });
    setState({ activeEnd: target === "origin" ? "destination" : "origin" });
    flyTo(p.lon, p.lat, 12);
    setOpen(false);
  };

  return (
    <div className="place-search" ref={boxRef}>
      <input
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onFocus={() => results.length && setOpen(true)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && results[0]) go(results[0]);
          if (e.key === "Escape") setOpen(false);
        }}
        placeholder="Search place or lon, lat"
        aria-label="Search place"
      />
      {busy && <span className="place-busy" />}
      {open && results.length > 0 && (
        <ul className="place-results">
          {results.map((r, i) => (
            <li key={`${r.lon}-${r.lat}-${i}`}>
              <button type="button" className="place-name" onClick={() => go(r)} title={r.label}>
                {r.label}
              </button>
              {routeTool && (
                <>
                  <button type="button" className="place-end" onClick={() => place("origin", r)} title="Use as start (A)">
                    A
                  </button>
                  <button type="button" className="place-end" onClick={() => place("destination", r)} title="Use as end (B)">
                    B
                  </button>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

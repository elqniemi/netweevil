import { ApiError, h3Api, type ApiResponse } from "../api/client";
import type { CellPaintRequest } from "../api/types";
import { planStroke, type LonLat, type StrokePlan } from "../geo/cells";
import { getState, pushTiming, setCells } from "./store";

export const CELLS_TOOL = "cells";

/** Runs one cell-layer call and logs its timing in the performance readout. */
export async function cellsCall<T>(send: (base: string) => Promise<ApiResponse<T>>): Promise<T> {
  try {
    const response = await send(getState().apiBase);
    pushTiming(response.timing);
    return response.data;
  } catch (error) {
    if (error instanceof ApiError) pushTiming(error.timing);
    throw error;
  }
}

let queue: Promise<void> = Promise.resolve();

/**
 * Paints one finished map stroke into the selected layer and column. Strokes
 * run one after the other in the order drawn; each ends with a refetch of
 * the layer.
 */
export function paintStroke(points: LonLat[], moved: boolean) {
  const { cells } = getState();
  const column = cells.column.trim();
  if (!cells.layerId || !column) {
    setCells({ error: cells.layerId ? "Choose a column." : "Choose a layer." });
    return;
  }
  const plan = planStroke(points, moved, { column, op: cells.op, value: cells.value, resolution: cells.resolution, mode: cells.paintMode }, cells.features?.features ?? []);
  if (!plan) return;
  const layerId = cells.layerId;
  setCells((prev) => ({ pending: prev.pending + 1, error: null }));
  queue = queue
    .then(() => runPlan(layerId, plan))
    .catch((error: unknown) => setCells({ error: error instanceof Error ? error.message : String(error) }))
    .finally(() => setCells((prev) => ({ pending: Math.max(0, prev.pending - 1), revision: prev.revision + 1 })));
}

async function runPlan(layerId: string, plan: StrokePlan): Promise<void> {
  if (plan.kind === "request") {
    await cellsCall((base) => h3Api.paint(base, CELLS_TOOL, layerId, plan.body));
    return;
  }
  if (!plan.cells.length && !plan.unresolved.length) return;
  // The API paints each distinct cell of `cells` and `points` once.
  const body: CellPaintRequest = { ...plan.base, cells: plan.cells, points: plan.unresolved, resolution: plan.resolution };
  await cellsCall((base) => h3Api.paint(base, CELLS_TOOL, layerId, body));
}

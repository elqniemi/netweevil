import { useQuery } from "@tanstack/react-query";
import { apiRequest } from "../api/client";
import type { ServiceInfo } from "../api/types";
import { useStore } from "./store";

/** Service info drives the profile/feed pickers and the dataset extent. */
export function useService() {
  const apiBase = useStore((s) => s.apiBase);
  return useQuery({
    queryKey: ["service", apiBase],
    queryFn: async () => (await apiRequest<ServiceInfo>(apiBase, "/v1/service", { tool: "service" })).data,
    staleTime: 60_000,
    retry: 1,
    refetchInterval: (query) => (query.state.status === "error" ? 5_000 : false),
  });
}

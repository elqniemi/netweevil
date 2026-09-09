import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./App";
import "./styles.css";

const queryClient = new QueryClient({
  defaultOptions: { queries: { refetchOnWindowFocus: false, retry: 0 } },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);

// Development hook for scripted checks from the browser console.
if (import.meta.env.DEV) {
  void Promise.all([import("./state/store"), import("./state/run")]).then(([store, run]) => {
    (window as unknown as { __netweevil: unknown }).__netweevil = { ...store, ...run };
  });
}

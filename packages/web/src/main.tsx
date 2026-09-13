import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { App } from "./app";
import { ApiError } from "./api/client";
import "./index.css";

// Retry transient reads only; rejected uploads keep their original idempotency key for explicit recovery.
const client = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 2000,
      retry: (count, error) =>
        count < 2 && (!(error instanceof ApiError) || error.status >= 500),
    },
  },
});
const root = document.getElementById("root");
if (!root) throw new Error("The workbench root is missing");
createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <BrowserRouter
        basename={import.meta.env.BASE_URL.replace(/\/$/, "") || "/"}
      >
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);

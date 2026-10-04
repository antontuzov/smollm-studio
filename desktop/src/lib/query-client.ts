import { QueryClient } from "@tanstack/react-query";

/**
 * One client for the app. Keeping it a module singleton means command helpers
 * can invalidate caches without threading the client through every handler.
 */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Local commands are cheap and deterministic: one retry is plenty, and a
      // window that just opened should not re-fetch everything it can see.
      retry: 1,
      staleTime: 15_000,
      refetchOnWindowFocus: false,
    },
  },
});

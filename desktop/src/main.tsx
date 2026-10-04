import { createRoot } from "react-dom/client";

import { App } from "./App";

import "@/styles/globals.css";

const container = document.getElementById("root");

if (!container) {
  throw new Error("SmolLLM Studio could not find its root element");
}

// No StrictMode here on purpose: its double subscription would attach every
// Tauri event listener twice, and streamed tokens would appear twice.
createRoot(container).render(<App />);

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyTheme, DEFAULT_THEME_MODE } from "./theme";
import "./theme/tokens.css";

const container = document.getElementById("root");
if (!container) throw new Error("missing #root");
applyTheme(document.documentElement, DEFAULT_THEME_MODE);
createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

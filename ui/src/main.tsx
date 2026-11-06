import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyTheme, DEFAULT_THEME_MODE } from "./theme";
import "./theme/tokens.css";
import "./fonts/fonts.css";
import { bootstrapFonts } from "./fonts";

const container = document.getElementById("root");
if (!container) throw new Error("missing #root");
applyTheme(document.documentElement, DEFAULT_THEME_MODE);
bootstrapFonts(document.documentElement, DEFAULT_THEME_MODE);

// Development-only gallery: `pnpm dev` then open http://localhost:1420/?gallery.
// The dynamic import keeps the gallery out of the release bundle.
const galleryRequested =
  import.meta.env.DEV && new URLSearchParams(window.location.search).has("gallery");

if (galleryRequested) {
  void import("./components/gallery").then(({ ComponentGallery }) => {
    createRoot(container).render(
      <StrictMode>
        <ComponentGallery />
      </StrictMode>,
    );
  });
} else {
  createRoot(container).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

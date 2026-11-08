import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  test: { environment: "jsdom", include: ["src/**/*.test.{ts,tsx}"] },
  build: {
    rolldownOptions: {
      output: {
        // Vendor split (TF-035): the entry chunk stays small so the shell
        // paints from ~150 kB of app code plus these stable vendor chunks;
        // heavy libraries (CodeMirror, markdown, diff, react-table) stay lazy
        // with the screens that are their only importers. The light component
        // kit shared by shell and screens forms rolldown's automatic shared
        // chunk, which stays small because the barrel in `components/index.ts`
        // does not re-export the heavy editor/diff/table widgets.
        codeSplitting: {
          groups: [
            { name: "react-vendor", test: /node_modules[\\/](react|react-dom|scheduler)[\\/]/ },
            { name: "radix-vendor", test: /node_modules[\\/]@radix-ui[\\/]/ },
            // The two heavy lazy libraries, split so neither crosses the
            // 500 kB chunk warning and each loads only for the screens that
            // use it: CodeMirror for the Instructions editor, the markdown
            // stack for previews and diffs (skill detail, drift). The
            // markdown language group is matched first so the CodeMirror core
            // stays under the warning threshold.
            { name: "codemirror-markdown", test: /node_modules[\\/]@codemirror[\\/]lang-markdown[\\/]|node_modules[\\/]@lezer[\\/]/ },
            { name: "codemirror-vendor", test: /node_modules[\\/]@codemirror[\\/]/ },
            { name: "markdown-vendor", test: /node_modules[\\/](react-markdown|rehype-sanitize|micromark|mdast|remark|unified|unist|vfile|hast-util|property-information|space-separated-tokens|comma-separated-tokens|trim-lines|zwitch|devlop|character-entities|decode-named)[\\/]/ },
          ],
        },
      },
    },
  },
});

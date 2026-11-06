# Theme manual checks (TU-001)

Automated checks in this directory cover token completeness, contrast ratios and
"no raw hex/font-size outside `ui/src/theme`". Two things need human eyes:

## 1. Runtime theme switch, shell + tables + overlays

1. `pnpm --dir ui dev`, open the app (or the Tauri window).
2. In the browser console (dev server) run:
   `const { applyTheme } = await import("/src/theme/index.ts");`
   `applyTheme(document.documentElement, "light");`
   Once a theme toggle exists, use it instead.
3. Confirm background, text, borders, focus rings, tables and any open overlay
   all change together; nothing keeps the old palette.
4. Repeat in the other direction: `applyTheme(document.documentElement, "dark")`.
   No reload should be needed.

## 2. No clipped labels at 1x and 2x

1. At 100% zoom, walk every screen: buttons, table headers, scope chips,
   status pills, tab labels. No glyph is cut off at the top, bottom or right.
2. Repeat at 200% zoom (browser zoom or the OS display setting).
3. Check the focus ring is fully visible, not clipped by a container edge, and
   that text does not overlap when a surface scrolls.

`layout-guards.test.ts` pins what can be automated: body line-height >= 1.3,
type sizes from the token scale, `html/body/#root` height 100% with no global
`overflow: hidden`, and focus rings drawn from `--at-color-focus`.

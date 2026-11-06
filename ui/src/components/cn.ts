/**
 * Joins class names, dropping falsy entries.
 *
 * Every class string in this library is written literally in the module that
 * uses it, so Tailwind's scanner can see it; `cn` only concatenates.
 */
export function cn(...parts: ReadonlyArray<string | false | null | undefined>): string {
  return parts.filter((part): part is string => Boolean(part)).join(" ");
}

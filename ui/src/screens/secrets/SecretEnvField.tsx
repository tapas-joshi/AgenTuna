import { useState } from "react";
import type { JSX } from "react";
import { SecretPicker } from "./SecretPicker";

/** MCP environment row editor that stores only a `${NAME}` reference. */
export function SecretEnvField({ name, reference, onChange }: {
  readonly name: string;
  readonly reference: string;
  readonly onChange: (reference: string) => void;
}): JSX.Element {
  const [pickerOpen, setPickerOpen] = useState(false);
  return <div className="flex items-center gap-8"><span className="min-w-[120px] font-mono text-12 text-text">{name}</span><code className="min-w-0 flex-1 truncate rounded-6 bg-surface-muted px-8 py-6 text-12 text-muted">{reference || "Choose a Keychain secret"}</code><button type="button" aria-label={`Choose secret for ${name}`} onClick={() => setPickerOpen(true)} className="rounded-8 border border-border px-10 py-7 text-12">Choose secret</button>{pickerOpen && <SecretPicker onSelect={onChange} onClose={() => setPickerOpen(false)} />}</div>;
}
